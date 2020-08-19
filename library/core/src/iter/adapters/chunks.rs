use crate::mem::MaybeUninit;

#[derive(PartialEq, Eq)]
enum ExtraHandling {
  Ignore,
}

/// An iterator over exact sized chunks in a nested iterator
struct ChunksCompat<I, T, const N: usize, const E: ExtraHandling> {
  // The buffer will never handle dropping
  buffer: [MaybeUninit<T>; N],
  iter: I,
}

pub struct ChunksExact<I, T, const N: usize> {
  inner: ChunksCompat<I, T, N, { ExtraHandling::Ignore }>,
}

impl<I, T, const N: usize> Iterator for ChunksCompat<I, T, N, { ExtraHandling::Ignore }> where I: Iterator<Item=T> {
  type Item = [T; N];
  fn next(&mut self) -> Option<Self::Item> {
    for i in 0..N {
      self.buffer[i] = match self.iter.next() {
        Some(v) => MaybeUninit::new(v),

        None => {
          // Have to drop all earlier items
          for j in 0..i {
            unsafe {
              self.buffer[j].read();
            }
          }
          return None;
        },
      };
    }
    let value = unsafe { crate::mem::transmute_copy::<_, [T; N]>(&self.buffer) };
    Some(value)
  }
}

