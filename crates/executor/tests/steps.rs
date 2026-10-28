use portcullis_executor::{step, ItemStatus::*, Step};

#[test]
fn nothing_that_might_have_left_is_released() {
    // Before settling, only statuses meaning "never left / net zero" release.
    for s in [Success, Unclaimed] {
        assert!(matches!(step(s, false), Step::Settle { .. }), "{s:?}");
    }
    for s in [Pending, OnHold] {
        assert_eq!(step(s, false), Step::Wait, "{s:?}");
    }
}

#[test]
fn after_settling_only_a_return_moves_money() {
    for s in [Returned, Refunded, Reversed] {
        assert_eq!(step(s, true), Step::CreditBack, "{s:?}");
    }
    assert_eq!(step(Success, true), Step::Done);
    assert_eq!(step(Unclaimed, true), Step::Wait);
    for s in [Failed, Blocked, Pending, OnHold] {
        assert_eq!(step(s, true), Step::Anomaly, "{s:?}");
    }
}
