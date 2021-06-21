use crate::transform::MirPass;
use crate::util::patch::MirPatch;
use rustc_data_structures::stable_map::FxHashMap;
use rustc_middle::mir::*;
use rustc_middle::ty::{self, Const, List, Ty, TyCtxt};
use rustc_span::def_id::DefId;
use rustc_target::abi::{Size, Variants};

/// A pass that seeks to optimize unnecessary moves of large enum types, if there is a large
/// enough discrepanc between them
pub struct EnumSizeOpt<const DISCREPANCY: u64>;

impl<'tcx, const D: u64> MirPass<'tcx> for EnumSizeOpt<D> {
    fn run_pass(&self, tcx: TyCtxt<'tcx>, body: &mut Body<'tcx>) {
        self.optim(tcx, body);
    }
}

impl<const D: u64> EnumSizeOpt<D> {
    fn candidate<'tcx>(
        tcx: TyCtxt<'tcx>,
        ty: Ty<'tcx>,
        body_did: DefId,
    ) -> Option<(Size, u64, Vec<Size>)> {
        match ty.kind() {
            ty::Adt(adt_def, _substs) if adt_def.is_enum() => {
                let p_e = tcx.param_env(body_did);
                // FIXME(jknodt) handle error better below
                let layout = tcx.layout_of(p_e.and(ty)).unwrap();
                let variants = &layout.variants;
                match variants {
                    Variants::Single { .. } => None,
                    Variants::Multiple { variants, .. } if variants.len() <= 1 => None,
                    Variants::Multiple { variants, .. } => {
                        let min = variants.iter().map(|v| v.size).min().unwrap();
                        let max = variants.iter().map(|v| v.size).max().unwrap();
                        if max.bytes() - min.bytes() < D {
                            return None;
                        }
                        Some((
                            layout.size,
                            variants.len() as u64,
                            variants.iter().map(|v| v.size).collect(),
                        ))
                    }
                }
            }
            _ => None,
        }
    }
    fn optim(&self, tcx: TyCtxt<'tcx>, body: &mut Body<'tcx>) {
        let mut match_cache = FxHashMap::default();
        let body_did = body.source.def_id();
        let mut patch = MirPatch::new(body);
        let (bbs, local_decls) = body.basic_blocks_and_local_decls_mut();
        for bb in bbs {
            bb.expand_statements(|st| {
                match &st.kind {
                    StatementKind::Assign(box (
                        lhs,
                        Rvalue::Use(Operand::Copy(rhs) | Operand::Move(rhs)),
                    )) => {
                        let ty = lhs.ty(local_decls, tcx).ty;
                        let (total_size, num_variants, sizes) =
                            if let Some((ts, nv, s)) = match_cache.get(ty) {
                                (*ts, *nv, s)
                            } else if let Some((ts, nv, s)) = Self::candidate(tcx, ty, body_did) {
                                // FIXME(jknodt) use entry API.
                                match_cache.insert(ty, (ts, nv, s));
                                let (ts, nv, s) = match_cache.get(ty).unwrap();
                                (*ts, *nv, s)
                            } else {
                                return None;
                            };

                        let source_info = st.source_info;
                        let span = source_info.span;

                        let tmp_ty = tcx.mk_ty(ty::Array(
                            tcx.types.usize,
                            Const::from_usize(tcx, num_variants),
                        ));

                        let new_local = patch.new_temp(tmp_ty, span);
                        let store_live =
                            Statement { source_info, kind: StatementKind::StorageLive(new_local) };

                        let place = Place { local: new_local, projection: List::empty() };
                        let mut data =
                            vec![0; std::mem::size_of::<usize>() * num_variants as usize];
                        data.copy_from_slice(unsafe { std::mem::transmute(&sizes[..]) });
                        let alloc = interpret::Allocation::from_bytes(
                            data,
                            tcx.data_layout.ptr_sized_integer().align(&tcx.data_layout).abi,
                        );
                        let alloc = tcx.intern_const_alloc(alloc);
                        let constant_vals = Constant {
                            span,
                            user_ty: None,
                            literal: ConstantKind::Val(
                                interpret::ConstValue::ByRef { alloc, offset: Size::ZERO },
                                tmp_ty,
                            ),
                        };
                        let rval = Rvalue::Use(Operand::Constant(box (constant_vals)));

                        let const_assign = Statement {
                            source_info,
                            kind: StatementKind::Assign(box (place, rval)),
                        };

                        // FIXME(jknodt) do I need to add a storage live here for this place?
                        let discr_place = Place {
                            local: patch.new_temp(tcx.types.usize, span),
                            projection: List::empty(),
                        };

                        let store_discr = Statement {
                            source_info,
                            kind: StatementKind::Assign(box (
                                discr_place,
                                Rvalue::Discriminant(*rhs),
                            )),
                        };

                        // FIXME(jknodt) do I need to add a storage live here for this place?
                        let size_place = Place {
                            local: patch.new_temp(tcx.types.usize, span),
                            projection: List::empty(),
                        };

                        let store_size = Statement {
                            source_info,
                            kind: StatementKind::Assign(box (
                                size_place,
                                Rvalue::Use(Operand::Copy(Place {
                                    local: discr_place.local,
                                    projection: tcx
                                        .intern_place_elems(&[PlaceElem::Index(size_place.local)]),
                                })),
                            )),
                        };

                        // FIXME(jknodt) do I need to add a storage live here for this place?
                        let dst = Place {
                            local: patch.new_temp(tcx.mk_mut_ptr(tcx.types.u8), span),
                            projection: List::empty(),
                        };

                        let dst_ptr = Statement {
                            source_info,
                            kind: StatementKind::Assign(box (
                                dst,
                                Rvalue::AddressOf(Mutability::Mut, *lhs),
                            )),
                        };

                        // FIXME(jknodt) do I need to add a storage live here for this place?
                        let src = Place {
                            local: patch.new_temp(tcx.mk_imm_ptr(tcx.types.u8), span),
                            projection: List::empty(),
                        };

                        let src_ptr = Statement {
                            source_info,
                            kind: StatementKind::Assign(box (
                                src,
                                Rvalue::AddressOf(Mutability::Mut, *rhs),
                            )),
                        };

                        let copy_bytes = Statement {
                            source_info,
                            kind: StatementKind::CopyNonOverlapping(box CopyNonOverlapping {
                                src: Operand::Copy(src),
                                dst: Operand::Copy(src),
                                count: Operand::Constant(
                                    box (Constant {
                                        span,
                                        user_ty: None,
                                        literal: ConstantKind::Val(
                                            interpret::ConstValue::from_u64(total_size.bytes()),
                                            tcx.types.usize,
                                        ),
                                    }),
                                ),
                            }),
                        };

                        let store_dead =
                            Statement { source_info, kind: StatementKind::StorageDead(new_local) };
                        let iter = std::array::IntoIter::new([
                            store_live,
                            const_assign,
                            store_discr,
                            store_size,
                            dst_ptr,
                            src_ptr,
                            copy_bytes,
                            store_dead,
                        ]);

                        st.make_nop();
                        Some(iter)
                    }
                    _ => return None,
                }
            });
        }
        patch.apply(body);
    }
}
