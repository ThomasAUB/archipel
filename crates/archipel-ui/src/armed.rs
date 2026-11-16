//! Buttons confirmed by a second click.
//!
//! A destructive button (Discard, Delete branches, Remove anyway, Merge,
//! Force push) is armed by a first click and runs on a second click on the
//! same thing within [`ARM_TIME`]; then it disarms by itself. Each arming
//! has its own timer: an older timer never disarms a newer arming.

use std::time::Duration;

use gpui_kit::Context;

/// How long an armed button waits for its confirming click.
pub const ARM_TIME: Duration = Duration::from_secs(4);

/// What is armed, if anything: a key naming what the second click must
/// land on (a file, a pull request; `()` for a lone button).
#[derive(Debug)]
pub struct Armed<K> {
    armed: Option<(K, u64)>,
    count: u64,
}

impl<K> Default for Armed<K> {
    fn default() -> Self {
        Self {
            armed: None,
            count: 0,
        }
    }
}

impl<K: Clone + PartialEq + 'static> Armed<K> {
    /// What is armed.
    pub fn get(&self) -> Option<&K> {
        self.armed.as_ref().map(|(k, _)| k)
    }

    /// Whether `key` is armed.
    pub fn is(&self, key: &K) -> bool {
        self.get() == Some(key)
    }

    /// Disarms (the selection it applied to changed, the dialog moved on).
    pub fn disarm(&mut self) {
        self.armed = None;
    }

    /// Takes what is armed, disarming it.
    pub fn take(&mut self) -> Option<K> {
        self.armed.take().map(|(k, _)| k)
    }

    /// A click on the button of `key`: `true` when it confirms (`key` was
    /// armed, and is no longer), else arms `key` for [`ARM_TIME`].
    /// `slot` finds this `Armed` in the view again when the time is up.
    pub fn click<V: 'static>(
        &mut self,
        key: K,
        cx: &mut Context<V>,
        slot: fn(&mut V) -> &mut Self,
    ) -> bool {
        if self.is(&key) {
            self.armed = None;
            return true;
        }
        self.arm(key, cx, slot);
        false
    }

    /// Arms `key` for [`ARM_TIME`], whatever was armed.
    pub fn arm<V: 'static>(&mut self, key: K, cx: &mut Context<V>, slot: fn(&mut V) -> &mut Self) {
        self.count += 1;
        let arming = self.count;
        self.armed = Some((key, arming));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ARM_TIME).await;
            let _ = this.update(cx, |v, cx| {
                let armed = slot(v);
                if armed.armed.as_ref().is_some_and(|(_, n)| *n == arming) {
                    armed.armed = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}
