use serde::{Deserialize, Serialize};

/// One source-node replacement proposed by an assistant tool. The application
/// owns the derived publication layer and transaction that applies it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockRewrite {
    pub section_index: usize,
    pub block_id: String,
    pub text: String,
}

/// One normalized block sent to a translation provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationBlockInput {
    pub block_index: usize,
    pub segment_index: Option<usize>,
    pub text: String,
}

/// Translation returned for one normalized block or one fixed-page segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockTranslation {
    pub block_index: usize,
    pub segment_index: Option<usize>,
    pub text: String,
}

/// Presentation policy for a derived translation source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranslationMode {
    #[default]
    Replace,
    Bilingual,
}

/// Persistence-neutral annotation mutation staged by an assistant response.
///
/// The payload type belongs to the application domain. Both desktop frontends
/// currently use the same sync-layer `StoredHighlight`, while this crate stays
/// independent of a database or UI toolkit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssistantAnnotationAction<T> {
    Create(T),
    Update(T),
    Delete { annotation_id: String },
}

/// Application adapter used to atomically confirm assistant annotation actions.
/// Each successful application returns an opaque undo value. The shared
/// coordinator rolls those values back in reverse order after any later error.
pub trait AssistantAnnotationTarget<T> {
    type Undo;

    fn apply_annotation(
        &mut self,
        action: &AssistantAnnotationAction<T>,
    ) -> Result<Self::Undo, String>;

    fn rollback_annotation(&mut self, undo: Self::Undo) -> Result<(), String>;
}

/// Outcome exposed to either frontend after resolving a staged mutation batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistantMutationResolution {
    Confirmed { applied: usize },
    Cancelled { discarded: usize },
}

/// One frontend-neutral batch of assistant annotation actions waiting for an
/// explicit user decision.
///
/// A failed confirmation deliberately keeps the actions intact so the UI can
/// report the error and allow a retry or cancellation. Successful confirmation
/// and cancellation are the only operations that clear the batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingAnnotationActions<T> {
    actions: Vec<AssistantAnnotationAction<T>>,
}

impl<T> Default for PendingAnnotationActions<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> PendingAnnotationActions<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            actions: Vec::new(),
        }
    }

    #[must_use]
    pub fn from_actions(actions: Vec<AssistantAnnotationAction<T>>) -> Self {
        Self { actions }
    }

    #[must_use]
    pub fn actions(&self) -> &[AssistantAnnotationAction<T>] {
        &self.actions
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.actions.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn replace(&mut self, actions: Vec<AssistantAnnotationAction<T>>) {
        self.actions = actions;
    }

    pub fn push(&mut self, action: AssistantAnnotationAction<T>) {
        self.actions.push(action);
    }

    pub fn confirm<H>(&mut self, target: &mut H) -> Result<AssistantMutationResolution, String>
    where
        H: AssistantAnnotationTarget<T>,
    {
        let resolution = confirm_annotation_actions(target, &self.actions)?;
        self.actions.clear();
        Ok(resolution)
    }

    pub fn cancel(&mut self) -> AssistantMutationResolution {
        cancel_annotation_actions(&mut self.actions)
    }

    #[must_use]
    pub fn into_actions(self) -> Vec<AssistantAnnotationAction<T>> {
        self.actions
    }
}

/// Atomically confirms annotation actions against an application-owned target.
/// If one action fails, all earlier actions are rolled back in reverse order.
pub fn confirm_annotation_actions<T, H>(
    target: &mut H,
    actions: &[AssistantAnnotationAction<T>],
) -> Result<AssistantMutationResolution, String>
where
    H: AssistantAnnotationTarget<T>,
{
    let mut undo = Vec::with_capacity(actions.len());
    for action in actions {
        match target.apply_annotation(action) {
            Ok(applied) => undo.push(applied),
            Err(error) => {
                let rollback_errors = rollback_annotations(target, undo);
                return if rollback_errors.is_empty() {
                    Err(error)
                } else {
                    Err(format!(
                        "{error}；回滚已应用的批注动作失败：{}",
                        rollback_errors.join("；")
                    ))
                };
            }
        }
    }
    Ok(AssistantMutationResolution::Confirmed {
        applied: actions.len(),
    })
}

/// Discards a pending annotation batch without touching persistence.
pub fn cancel_annotation_actions<T>(
    actions: &mut Vec<AssistantAnnotationAction<T>>,
) -> AssistantMutationResolution {
    let discarded = actions.len();
    actions.clear();
    AssistantMutationResolution::Cancelled { discarded }
}

fn rollback_annotations<T, H>(target: &mut H, undo: Vec<H::Undo>) -> Vec<String>
where
    H: AssistantAnnotationTarget<T>,
{
    undo.into_iter()
        .rev()
        .filter_map(|undo| target.rollback_annotation(undo).err())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryTarget {
        values: Vec<i32>,
        fail_on: Option<i32>,
        rollbacks: Vec<i32>,
    }

    impl AssistantAnnotationTarget<i32> for MemoryTarget {
        type Undo = i32;

        fn apply_annotation(
            &mut self,
            action: &AssistantAnnotationAction<i32>,
        ) -> Result<Self::Undo, String> {
            let AssistantAnnotationAction::Create(value) = action else {
                return Err("unsupported test action".into());
            };
            if self.fail_on == Some(*value) {
                return Err(format!("failed on {value}"));
            }
            self.values.push(*value);
            Ok(*value)
        }

        fn rollback_annotation(&mut self, undo: Self::Undo) -> Result<(), String> {
            let value = self
                .values
                .pop()
                .ok_or_else(|| "missing applied value".to_owned())?;
            if value != undo {
                return Err("undo order changed".into());
            }
            self.rollbacks.push(value);
            Ok(())
        }
    }

    #[test]
    fn confirmation_rolls_back_prior_actions_after_a_later_failure() {
        let actions = vec![
            AssistantAnnotationAction::Create(1),
            AssistantAnnotationAction::Create(2),
            AssistantAnnotationAction::Create(3),
        ];
        let mut target = MemoryTarget {
            fail_on: Some(3),
            ..MemoryTarget::default()
        };

        assert_eq!(
            confirm_annotation_actions(&mut target, &actions).unwrap_err(),
            "failed on 3"
        );
        assert!(target.values.is_empty());
        assert_eq!(target.rollbacks, vec![2, 1]);
    }

    #[test]
    fn successful_confirmation_reports_the_applied_count() {
        let actions = vec![
            AssistantAnnotationAction::Create(1),
            AssistantAnnotationAction::Create(2),
        ];
        let mut target = MemoryTarget::default();

        assert_eq!(
            confirm_annotation_actions(&mut target, &actions).unwrap(),
            AssistantMutationResolution::Confirmed { applied: 2 }
        );
        assert_eq!(target.values, vec![1, 2]);
    }

    #[test]
    fn cancellation_discards_actions_without_calling_a_target() {
        let mut actions = vec![AssistantAnnotationAction::Create(1)];
        assert_eq!(
            cancel_annotation_actions(&mut actions),
            AssistantMutationResolution::Cancelled { discarded: 1 }
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn pending_batch_is_retained_after_failed_confirmation() {
        let mut pending = PendingAnnotationActions::from_actions(vec![
            AssistantAnnotationAction::Create(1),
            AssistantAnnotationAction::Create(2),
        ]);
        let mut target = MemoryTarget {
            fail_on: Some(2),
            ..MemoryTarget::default()
        };

        assert!(pending.confirm(&mut target).is_err());
        assert_eq!(pending.len(), 2);
        assert!(target.values.is_empty());
    }

    #[test]
    fn pending_batch_clears_only_after_resolution() {
        let mut confirmed = PendingAnnotationActions::from_actions(vec![
            AssistantAnnotationAction::Create(1),
            AssistantAnnotationAction::Create(2),
        ]);
        let mut target = MemoryTarget::default();
        assert_eq!(
            confirmed.confirm(&mut target).unwrap(),
            AssistantMutationResolution::Confirmed { applied: 2 }
        );
        assert!(confirmed.is_empty());

        let mut cancelled =
            PendingAnnotationActions::from_actions(vec![AssistantAnnotationAction::Create(3)]);
        assert_eq!(
            cancelled.cancel(),
            AssistantMutationResolution::Cancelled { discarded: 1 }
        );
        assert!(cancelled.is_empty());
    }
}
