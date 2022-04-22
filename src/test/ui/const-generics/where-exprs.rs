// TODO feature gate
pub struct Test<const N:usize> where const { N % 2 == 0 } {
  vals: [u8; N],
}

pub fn test<const N: usize>() -> usize where const { N == 3 } {
  N
}

fn main() {}
