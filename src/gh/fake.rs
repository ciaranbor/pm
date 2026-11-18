//! A stand-in for `gh` in tests: while a [`Fake`] is alive, every `gh`
//! call on its thread goes to its closure instead of the network.

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::Result;

type Respond = Box<dyn FnMut(&[&str]) -> Result<String>>;
type Calls = Rc<RefCell<Vec<Vec<String>>>>;

thread_local! {
    static FAKE: RefCell<Option<(Respond, Calls)>> = const { RefCell::new(None) };
}

/// The installed fake; dropping it restores the real `gh`.
pub(crate) struct Fake {
    calls: Calls,
}

/// Answer this thread's `gh` calls with `respond`, given each call's args.
pub(crate) fn install(respond: impl FnMut(&[&str]) -> Result<String> + 'static) -> Fake {
    let calls = Rc::new(RefCell::new(Vec::new()));
    FAKE.with(|fake| *fake.borrow_mut() = Some((Box::new(respond), calls.clone())));
    Fake { calls }
}

impl Fake {
    /// The args of every call answered so far, in order.
    pub(crate) fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        FAKE.with(|fake| *fake.borrow_mut() = None);
    }
}

pub(super) fn reply(args: &[&str]) -> Option<Result<String>> {
    FAKE.with(|fake| {
        let mut fake = fake.borrow_mut();
        let (respond, calls) = fake.as_mut()?;
        calls
            .borrow_mut()
            .push(args.iter().map(|a| a.to_string()).collect());
        Some(respond(args))
    })
}
