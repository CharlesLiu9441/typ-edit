//! Atomic edit operations recorded in the undo/redo history.

/// A single applied edit, or an undo-group separator flag.
///
/// All indices are **char** indices. [`Operation::inverse`] swaps Insert and
/// Delete while keeping `at` and `text` unchanged: the text spans the same char
/// range in both directions, so no surrounding context is needed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    /// Insert `text` at char index `at`.
    Insert {
        /// Char index the text was inserted at.
        at: usize,
        /// The inserted text.
        text: String,
    },
    /// Delete `text` that used to occupy `[at, at + text.chars().count())`.
    Delete {
        /// Char index the removed range started at.
        at: usize,
        /// The removed text.
        text: String,
    },
    /// Separator between undo groups; never passed to edit hooks.
    UndoRedoFlag,
}

impl Operation {
    /// Returns the operation that reverses `self`.
    ///
    /// Insert and Delete swap while keeping `at` and `text` unchanged; the flag
    /// maps to itself.
    pub fn inverse(&self) -> Operation {
        match self {
            Operation::Insert { at, text } => Operation::Delete {
                at: *at,
                text: text.clone(),
            },
            Operation::Delete { at, text } => Operation::Insert {
                at: *at,
                text: text.clone(),
            },
            Operation::UndoRedoFlag => Operation::UndoRedoFlag,
        }
    }

    /// Returns `true` for [`Operation::UndoRedoFlag`].
    pub fn is_flag(&self) -> bool {
        matches!(self, Operation::UndoRedoFlag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_swaps_insert_and_delete() {
        let insert = Operation::Insert {
            at: 3,
            text: "hi\n".into(),
        };
        let delete = insert.inverse();
        assert_eq!(
            delete,
            Operation::Delete {
                at: 3,
                text: "hi\n".into(),
            }
        );
        assert_eq!(delete.inverse(), insert);
    }

    #[test]
    fn flag_is_its_own_inverse() {
        let flag = Operation::UndoRedoFlag;
        assert!(flag.is_flag());
        assert_eq!(flag.inverse(), flag);
        assert!(
            !Operation::Delete {
                at: 0,
                text: String::new(),
            }
            .is_flag()
        );
    }
}
