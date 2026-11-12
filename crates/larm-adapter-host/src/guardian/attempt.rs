//! Registered metadata plus a live native witness, never a persisted closed flag.
use super::*;
use larm_domain::{
    transformation_execution::{TransformationAttemptGuard, TransformationReadyOwner},
    transformations::TransformationExecutionBinding,
};

pub struct NativeTransformationAttempt {
    observer: NativeGuardian,
    owner: TransformationNativeOwner,
}
impl NativeTransformationAttempt {
    pub async fn attach(owner: TransformationNativeOwner) -> Result<Self, LarmError> {
        let descriptor = GuardianDescriptor::from_native_owner(owner.clone())?;
        let observer = NativeGuardian::attach(descriptor).await?;
        Ok(Self { observer, owner })
    }
    pub fn observer(&self) -> &NativeGuardian {
        &self.observer
    }
    pub async fn release(self) -> Result<(), LarmError> {
        self.observer.release().await
    }
}
#[async_trait::async_trait]
impl TransformationAttemptGuard for NativeTransformationAttempt {
    fn fence(&self) -> &TransformationAttemptFence {
        &self.owner.fence
    }
    fn binding(&self) -> &TransformationExecutionBinding {
        &self.owner.binding
    }
    fn native_owner(&self) -> Option<&TransformationNativeOwner> {
        Some(&self.owner)
    }
    async fn recheck(&self) -> Result<(), LarmError> {
        self.observer.check(Action::Recheck).await
    }
    async fn require_closed(&self) -> Result<(), LarmError> {
        self.observer.require_closed().await
    }
}
#[async_trait::async_trait]
impl TransformationReadyOwner for NativeTransformationAttempt {
    async fn require_ready(&self) -> Result<(), LarmError> {
        self.observer.check(Action::Ready).await
    }
}
