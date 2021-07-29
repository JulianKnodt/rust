// The classification code for the x86_64 ABI is taken from the clay language
// https://github.com/jckarter/clay/blob/master/compiler/externals.cpp

use crate::abi::call::{ArgAbi, CastTarget, FnAbi, Reg, RegKind};
use crate::abi::{self, Abi, HasDataLayout, LayoutOf, Size, TyAndLayout, TyAndLayoutMethods};

/// Classification of "eightbyte" components.
// N.B., the order of the variants is from general to specific,
// such that `unify(a, b)` is the "smaller" of `a` and `b`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Class {
    /// Int represents a scalar integer
    Int,
    /// Sse represents the lower half or start of an SSE vector
    Sse,
    /// SseUp represents the upper half of an SSE register
    SseUp,
}

/// Memory is an error value indicating that arguments must be passed on stack, and cannot be
/// used in registers.
#[derive(Clone, Copy, Debug)]
struct Memory;

// Currently supported vector size (AVX-512).
const LARGEST_VECTOR_SIZE: usize = 512;
const MAX_EIGHTBYTES: usize = LARGEST_VECTOR_SIZE / 64;

/// Classify classifies cx as either an x86 class, or returns an error indicating that it must
/// be passed through memory.
fn classify<'a, Ty, C>(
    cx: &C,
    layout: TyAndLayout<'a, Ty>,
    cls: &mut [Option<Class>],
    offset: Size,
) -> Result<(), Memory>
where
    Ty: TyAndLayoutMethods<'a, C> + Copy,
    C: LayoutOf<Ty = Ty, TyAndLayout = TyAndLayout<'a, Ty>> + HasDataLayout,
{
    if !offset.is_aligned(layout.align.abi) {
        if !layout.is_zst() {
            return Err(Memory);
        }
        return Ok(());
    }

    let mut c = match layout.abi {
        Abi::Uninhabited => return Ok(()),

        Abi::Scalar(ref scalar) => match scalar.value {
            abi::Int(..) | abi::Pointer => Class::Int,
            abi::F32 | abi::F64 => Class::Sse,
        },

        Abi::Vector { .. } => Class::Sse,

        Abi::ScalarPair(..) | Abi::Aggregate { .. } => {
            for i in 0..layout.fields.count() {
                let field_off = offset + layout.fields.offset(i);
                classify(cx, layout.field(cx, i), cls, field_off)?;
            }

            match &layout.variants {
                abi::Variants::Single { .. } => {}
                abi::Variants::Multiple { variants, .. } => {
                    // Treat enum variants like union members.
                    for variant_idx in variants.indices() {
                        classify(cx, layout.for_variant(cx, variant_idx), cls, offset)?;
                    }
                }
            }

            return Ok(());
        }
    };

    // Fill in `cls` for scalars (Int/Sse) and vectors (Sse).
    let first = (offset.bytes() / 8) as usize;
    let last = ((offset.bytes() + layout.size.bytes() - 1) / 8) as usize;
    for cl in &mut cls[first..=last] {
        *cl = Some(cl.map_or(c, |old| old.min(c)));

        // Everything after the first Sse "eightbyte"
        // component is the upper half of a register.
        if c == Class::Sse {
            c = Class::SseUp;
        }
    }

    Ok(())
}

// classifies an argument, returning either a sized register, or indicates that it must be in
// memory.
fn classify_arg<'a, Ty, C>(
    cx: &C,
    arg: &ArgAbi<'a, Ty>,
) -> Result<[Option<Class>; MAX_EIGHTBYTES], Memory>
where
    Ty: TyAndLayoutMethods<'a, C> + Copy,
    C: LayoutOf<Ty = Ty, TyAndLayout = TyAndLayout<'a, Ty>> + HasDataLayout,
{
    let n = ((arg.layout.size.bytes() + 7) / 8) as usize;
    if n > MAX_EIGHTBYTES {
        return Err(Memory);
    } else if n == 0 {
        return Ok([None; MAX_EIGHTBYTES]);
    }

    let mut cls = [None; MAX_EIGHTBYTES];
    classify(cx, arg.layout, &mut cls, Size::ZERO)?;
    if n > 2 {
        if cls[0] != Some(Class::Sse) {
            return Err(Memory);
        }
        if cls[1..n].iter().any(|&c| c != Some(Class::SseUp)) {
            return Err(Memory);
        }
        return Ok(cls);
    }

    let mut i = 0;
    while i < n {
        match cls[i] {
            Some(Class::SseUp) => cls[i] = Some(Class::Sse),
            Some(Class::Sse) => {
                i += 1;
                while i < n && cls[i] == Some(Class::SseUp) {
                    i += 1;
                }
            }
            None | Some(Class::Int) => i += 1,
        }
    }

    Ok(cls)
}

// converts x86 classes to the LLVM register ABI
fn reg_component(cls: &[Option<Class>], i: &mut usize, size: Size) -> Option<Reg> {
    if *i >= cls.len() {
        return None;
    }

    match cls[*i] {
        None => None,
        Some(Class::Int) => {
            *i += 1;
            Some(if size.bytes() < 8 { Reg { kind: RegKind::Integer, size } } else { Reg::i64() })
        }
        Some(Class::Sse) => {
            let vec_len =
                1 + cls[*i + 1..].iter().take_while(|&&c| c == Some(Class::SseUp)).count();
            *i += vec_len;
            Some(if vec_len == 1 {
                match size.bytes() {
                    4 => Reg::f32(),
                    _ => Reg::f64(),
                }
            } else {
                Reg { kind: RegKind::Vector, size: Size::from_bytes(8) * (vec_len as u64) }
            })
        }
        Some(c) => unreachable!("reg_component: unhandled class {:?}", c),
    }
}

fn cast_target(cls: &[Option<Class>], size: Size) -> CastTarget {
    let mut i = 0;
    let lo = reg_component(cls, &mut i, size).unwrap();
    let mut target = CastTarget::from(lo);
    let offset = Size::from_bytes(8) * (i as u64);
    if size > offset {
        if let Some(hi) = reg_component(cls, &mut i, size - offset) {
            target = CastTarget::pair(lo, hi);
        }
    }
    assert_eq!(reg_component(cls, &mut i, Size::ZERO), None);
    target
}

const MAX_INT_REGS: usize = 6; // RDI, RSI, RDX, RCX, R8, R9
const MAX_SSE_REGS: usize = 8; // XMM0-7

// the information about C is returned in FnAbi, notably information about LLVM types returned,
// and the arguments to functions.
pub fn compute_abi_info<'a, Ty, C>(cx: &C, fn_abi: &mut FnAbi<'a, Ty>)
where
    Ty: TyAndLayoutMethods<'a, C> + Copy,
    C: LayoutOf<Ty = Ty, TyAndLayout = TyAndLayout<'a, Ty>> + HasDataLayout,
{
    let mut int_regs = MAX_INT_REGS;
    let mut sse_regs = MAX_SSE_REGS;

    let mut x86_64_arg_or_ret = |arg: &mut ArgAbi<'a, Ty>, is_arg: bool| {
        let mut cls_or_mem = classify_arg(cx, arg);

        if is_arg {
            if let Ok(cls) = cls_or_mem {
                let mut needed_int = 0;
                let mut needed_sse = 0;
                for c in cls {
                    match c {
                        Some(Class::Int) => needed_int += 1,
                        Some(Class::Sse) => needed_sse += 1,
                        _ => {}
                    }
                }
                match (int_regs.checked_sub(needed_int), sse_regs.checked_sub(needed_sse)) {
                    (Some(left_int), Some(left_sse)) => {
                        int_regs = left_int;
                        sse_regs = left_sse;
                    }
                    _ => {
                        // Not enough registers for this argument, so it will be
                        // passed on the stack, but we only mark aggregates
                        // explicitly as indirect `byval` arguments, as LLVM will
                        // automatically put immediates on the stack itself.
                        if arg.layout.is_aggregate() {
                            cls_or_mem = Err(Memory);
                        }
                    }
                }
            }
        }

        match cls_or_mem {
            Err(Memory) => {
                if is_arg {
                    arg.make_indirect_byval();
                } else {
                    // `sret` parameter thus one less integer register available
                    arg.make_indirect();
                    // NOTE(eddyb) return is handled first, so no registers
                    // should've been used yet.
                    assert_eq!(int_regs, MAX_INT_REGS);
                    int_regs -= 1;
                }
            }
            Ok(ref cls) => {
                // split into sized chunks passed individually
                if arg.layout.is_aggregate() {
                    let size = arg.layout.size;
                    let target = cast_target(cls, size);
                    arg.cast_to(target);
                } else {
                    arg.extend_integer_width_to(32);
                }
            }
        }
    };

    if !fn_abi.ret.is_ignore() {
        x86_64_arg_or_ret(&mut fn_abi.ret, false);
    }

    for arg in &mut fn_abi.args {
        if arg.is_ignore() {
            continue;
        }
        x86_64_arg_or_ret(arg, true);
    }
}
