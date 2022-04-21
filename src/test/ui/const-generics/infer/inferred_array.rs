#![feature(generic_arg_infer)]
// run-pass

struct Example<const N: usize> {
  v: [u8; N],
}

fn main() {
    let arr : [u8; 8] = [0; _];
    assert!(arr.iter().all(|&v| v == 0));

    let arr : [u8; 3000] = [0; _];
    assert!(arr.iter().all(|&v| v == 0));

    let arr : [u8; 3000] = [0; _];
    assert!(arr.iter().all(|&v| v == 0));

    let [_a,_b,_c,_d] = [0;_];

    let ([_, _], [[_,_], [_, _]]) = ([0; _], [[true; _];_]);

    const N: usize = 3;
    let _: [usize; N] = [0; _];

    let ex = Example {
      v: [0; _],
    };
    let [_, _] = ex.v;
}


