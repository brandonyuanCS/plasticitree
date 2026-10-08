use crate::Result;

// Production builds contain no configurable fault behavior. Tests use a
// thread-local hook, so concurrent tests cannot affect another engine's IO.
pub(crate) fn hit(point: &'static str) -> Result<()> {
    #[cfg(test)]
    return testing::hit(point);
    #[cfg(not(test))]
    {
        let _ = point;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use crate::Result;
    use std::{cell::RefCell, io};
    thread_local! { static HOOK: RefCell<Option<(&'static str, bool)>> = const { RefCell::new(None) }; }
    pub fn set(point: &'static str, abort: bool) {
        HOOK.with(|h| *h.borrow_mut() = Some((point, abort)));
    }
    pub fn clear() {
        HOOK.with(|h| *h.borrow_mut() = None);
    }
    pub fn hit(point: &'static str) -> Result<()> {
        HOOK.with(|h| {
            if let Some((target, abort)) = *h.borrow()
                && target == point
            {
                if abort {
                    std::process::abort();
                }
                return Err(io::Error::other(format!("injected failure at {point}")).into());
            }
            Ok(())
        })
    }
}
