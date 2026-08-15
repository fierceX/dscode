use crate::agent::orchestrator::TurnStatus;
use crate::capabilities::{CapabilityExposure, RuntimeSkill, SkillDiscoveryPolicy};
use crate::runtime::TurnOutcome;
use crate::sdk_protocol::{
    PROTOCOL_VERSION, SdkCapabilityExposure, SdkFinal, SdkRequest, SdkSkillDiscoveryPolicy,
    SdkStatus, path_string,
};

pub fn runtime_skills_from_sdk_request(req: &SdkRequest) -> Vec<RuntimeSkill> {
    req.options
        .tools
        .inline_skills
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|skill| {
            RuntimeSkill::new(
                skill.name.clone(),
                skill.description.clone(),
                skill.content.clone(),
            )
            .with_exposure(capability_exposure_from_sdk(
                skill
                    .exposure
                    .unwrap_or(SdkCapabilityExposure::ModelAddressable),
            ))
            .with_optional_revision(skill.revision.clone())
        })
        .collect()
}

pub fn skill_discovery_policy_from_sdk_request(req: &SdkRequest) -> Option<SkillDiscoveryPolicy> {
    req.options
        .tools
        .skill_discovery_policy
        .map(skill_discovery_policy_from_sdk)
}

fn capability_exposure_from_sdk(exposure: SdkCapabilityExposure) -> CapabilityExposure {
    match exposure {
        SdkCapabilityExposure::ModelDiscoverable => CapabilityExposure::ModelDiscoverable,
        SdkCapabilityExposure::ModelAddressable => CapabilityExposure::ModelAddressable,
        SdkCapabilityExposure::HostOnly => CapabilityExposure::HostOnly,
    }
}

fn skill_discovery_policy_from_sdk(policy: SdkSkillDiscoveryPolicy) -> SkillDiscoveryPolicy {
    match policy {
        SdkSkillDiscoveryPolicy::Defaults => SkillDiscoveryPolicy::Defaults,
        SdkSkillDiscoveryPolicy::RuntimeOnly => SkillDiscoveryPolicy::RuntimeOnly,
        SdkSkillDiscoveryPolicy::ExplicitOnly => SkillDiscoveryPolicy::ExplicitOnly,
    }
}

pub fn sdk_status_from_turn(status: TurnStatus) -> SdkStatus {
    match status {
        TurnStatus::Ok => SdkStatus::Ok,
        TurnStatus::Failed => SdkStatus::Failed,
        TurnStatus::Interrupted => SdkStatus::Interrupted,
        TurnStatus::MaxTurnsExceeded => SdkStatus::MaxTurnsExceeded,
    }
}

pub fn exit_code_from_turn(status: TurnStatus) -> i32 {
    match status {
        TurnStatus::Ok => 0,
        TurnStatus::Failed => 1,
        TurnStatus::Interrupted => 130,
        TurnStatus::MaxTurnsExceeded => 2,
    }
}

pub fn final_from_outcome(outcome: &TurnOutcome) -> SdkFinal {
    let session = &outcome.session;
    SdkFinal {
        event_type: "final",
        version: PROTOCOL_VERSION,
        status: sdk_status_from_turn(outcome.status),
        billing_turn_id: outcome.billing_turn_id.clone(),
        session_id: session.session_id.clone(),
        session_ref: session.session_ref.clone(),
        home: path_string(&session.home),
        cwd: path_string(&session.cwd),
        events_path: path_string(&session.events_path),
        conversation_path: path_string(&session.conversation_path),
        artifacts_dir: path_string(&session.artifacts_dir),
        summary_path: path_string(&session.summary_path),
        usage_path: path_string(&session.usage_path),
        tool_call_count: outcome.tool_call_count,
        tool_error_count: outcome.tool_error_count,
        error: outcome.error.clone(),
        usage_records: outcome.usage_records.clone(),
        usage: outcome.usage.clone(),
    }
}

#[cfg(test)]
#[path = "sdk_adapter_tests.rs"]
mod tests;
