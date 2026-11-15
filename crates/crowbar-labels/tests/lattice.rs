//! Labels are ordered public, personal, private, and private data carries its
//! categories. Joining two labels gives the least label at or above both.

use crowbar_labels::{Category, Label, Site};

fn private(categories: &[Category]) -> Label {
    let mut label = Label::Personal;
    for category in categories {
        label = label.join(&Label::private(category.clone()));
    }
    label
}

#[test]
fn a_join_is_the_stricter_label_and_the_union_of_categories() {
    use Category::{Finance, Health, Messages};
    let bank = Category::Site(Site::new("bank.example"));

    // (left, right, join). Every expected value is worked out by hand.
    let cases = [
        (Label::Public, Label::Public, Label::Public),
        (Label::Public, Label::Personal, Label::Personal),
        (Label::Personal, Label::Public, Label::Personal),
        (Label::Personal, Label::Personal, Label::Personal),
        (Label::Public, private(&[Health]), private(&[Health])),
        (private(&[Health]), Label::Personal, private(&[Health])),
        (private(&[Messages]), private(&[Messages]), private(&[Messages])),
        (private(&[Messages]), private(&[Finance]), private(&[Finance, Messages])),
        (
            private(&[Finance, Messages]),
            private(&[Health]),
            private(&[Messages, Health, Finance]),
        ),
        (
            private(&[Messages]),
            private(&[bank.clone()]),
            private(&[bank.clone(), Messages]),
        ),
    ];
    for (left, right, expected) in cases {
        assert_eq!(left.join(&right), expected, "{left:?} join {right:?}");
        assert_eq!(
            right.join(&left),
            expected,
            "join is not symmetric for {left:?}, {right:?}"
        );
    }
}

#[test]
fn a_label_is_at_most_another_when_the_other_is_as_strict_in_every_way() {
    use Category::{Finance, Messages};

    // (lower, higher, lower is at most higher)
    let cases = [
        (Label::Public, Label::Personal, true),
        (Label::Personal, Label::Public, false),
        (Label::Personal, private(&[Messages]), true),
        (private(&[Messages]), Label::Personal, false),
        (private(&[Messages]), private(&[Messages, Finance]), true),
        (private(&[Messages, Finance]), private(&[Messages]), false),
        // Two different categories: neither is at most the other. A party
        // cleared for finance is not cleared for messages.
        (private(&[Messages]), private(&[Finance]), false),
        (private(&[Finance]), private(&[Messages]), false),
    ];
    for (left, right, expected) in cases {
        assert_eq!(left.is_at_most(&right), expected, "{left:?} at most {right:?}");
    }
}

#[test]
fn site_names_compare_without_regard_to_ascii_case() {
    assert_eq!(Site::new("Bank.Example"), Site::new("bank.example"));
}
