use aworkit_protocol::StableId;
use crate::{ApprovalRequirement, AuthorityManifest};
/// A worker-originated proposal with no direct host transport capability.
#[derive(Clone, Debug, Eq, PartialEq)] pub struct WorkerProposal { pub proposal_id: StableId, pub capability_id: StableId, pub payload_hash: String }
#[derive(Clone, Debug, Eq, PartialEq)] pub enum InvocationDecision { Denied, AwaitingApproval, Approved { invocation_id: StableId } }
/// Evaluates only frozen authority; dispatch remains a separately committed core step.
pub struct InvocationBroker;
impl InvocationBroker { pub fn decide(manifest: &AuthorityManifest, proposal: &WorkerProposal, approved: bool) -> InvocationDecision { let Some(binding) = manifest.capability_bindings.iter().find(|binding| binding.capability_id == proposal.capability_id) else { return InvocationDecision::Denied; }; if binding.approval == ApprovalRequirement::PerInvocation && !approved { return InvocationDecision::AwaitingApproval; } InvocationDecision::Approved { invocation_id: StableId::parse(format!("invoke.{}", proposal.proposal_id.as_str())).expect("proposal ids create valid invocation ids") } } }
