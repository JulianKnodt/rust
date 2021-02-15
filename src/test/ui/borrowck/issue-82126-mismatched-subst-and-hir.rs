// Regression test for #82087. Checks that mismatched lifetimes and types are
// properly handled.

// edition:2018

use std::sync::Mutex;

struct MarketMultiplier {}

impl MarketMultiplier {
    fn buy(&mut self) -> &mut usize {
        todo!()
    }
}

async fn buy_lock(generator: &Mutex<MarketMultiplier>) -> LockedMarket<'_> {
    //~^ ERROR this struct takes 0 lifetime arguments but 1 lifetime argument was supplied
    //~^^ ERROR this struct takes 1 type argument but 0 type arguments were supplied
    LockedMarket(generator.lock().unwrap().buy())
    //~^ ERROR cannot return value referencing temporary value
}

struct LockedMarket<T>(T);

fn main() {}
