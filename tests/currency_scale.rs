//! A separate allocator budget keeps accidental billion-address expansion bounded.
use second::{Operation, SecondState, StateStore};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
#[path = "integration/support/mod.rs"]
mod support;
use support::FinalizedExecute as _;

thread_local! { static ALLOCATED: Cell<Option<usize>> = const { Cell::new(None) }; }
struct BudgetAllocator;
fn charge(size: usize) -> bool {
    ALLOCATED
        .try_with(|counter| match counter.get() {
            Some(bytes) => {
                let bytes = bytes.saturating_add(size);
                counter.set(Some(bytes));
                bytes <= 32 * 1024 * 1024
            }
            None => true,
        })
        .unwrap_or(true)
}
unsafe impl GlobalAlloc for BudgetAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !charge(layout.size()) {
            return std::ptr::null_mut();
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if !charge(layout.size()) {
            return std::ptr::null_mut();
        }
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if !charge(size) {
            return std::ptr::null_mut();
        }
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: BudgetAllocator = BudgetAllocator;
fn bounded<T>(action: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ALLOCATED.set(None);
        }
    }
    ALLOCATED.set(Some(0));
    let _reset = Reset;
    action()
}

#[test]
fn billion_unit_issue_transfer_balance_and_snapshot_stay_bounded() {
    let alice = support::account(1);
    let bob = support::account(2);
    let source = support::payment(1);
    let destination = support::payment(2);
    let mut state = SecondState::genesis([alice, bob], 1);
    bounded(|| {
        state
            .execute_finalized(
                &support::verified_task(
                    1,
                    vec![
                        Operation::RegisterPaymentAddress {
                            address: source,
                            account: alice,
                        },
                        Operation::RegisterPaymentAddress {
                            address: destination,
                            account: bob,
                        },
                        Operation::Issue {
                            account: alice,
                            count: 1_000_000_000,
                        },
                    ],
                ),
                1,
            )
            .unwrap()
    });
    for (id, amount) in [(2, 1), (3, 1_000_000)] {
        bounded(|| {
            state
                .execute_finalized(
                    &support::verified_task(
                        id,
                        vec![Operation::Transfer {
                            source,
                            destination,
                            amount,
                        }],
                    ),
                    1,
                )
                .unwrap()
        });
    }
    bounded(|| {
        assert_eq!(state.balance(alice), 998_999_999);
        assert_eq!(state.balance(bob), 1_000_001);
        assert_eq!(state.current_supply(), 1_000_000_000);
        assert_eq!(
            state.public_currency_summary().current_supply,
            1_000_000_000
        );
        assert_eq!(state.public_currency_states().len(), 1);
    });
    let base = support::temp_base("currency-scale");
    let store = StateStore::new(&base);
    bounded(|| {
        store
            .initialize(&state, &support::validator_set(1, 1..=4))
            .unwrap();
        // A fresh store forces decoding instead of returning the write cache.
        let restored = StateStore::new(&base).load().unwrap().unwrap();
        assert_eq!(restored.state.balance(alice), state.balance(alice));
        assert_eq!(restored.state.balance(bob), state.balance(bob));
    });
    assert!(std::fs::metadata(base.with_extension("a")).unwrap().len() < 16 * 1024);
    store.remove_files().unwrap();
}
