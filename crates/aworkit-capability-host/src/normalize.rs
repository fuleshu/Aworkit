use aworkit_protocol::StableId;
/// Conservative final effect state; unknown never becomes retry-safe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)] pub enum OutcomeKind { Succeeded, FailedSafe, UnknownEffect }
#[derive(Clone, Debug, Eq, PartialEq)] pub struct CapabilityOutcome { pub invocation_id: StableId, pub kind: OutcomeKind, pub retry_safe: bool }
/// One redaction set shared by every provider/tool diagnostic path.
#[derive(Clone, Debug, Default)] pub struct Redactor { secrets: Vec<String> }
impl Redactor { pub fn new(secrets: Vec<String>) -> Self { Self { secrets: secrets.into_iter().filter(|value| !value.is_empty()).collect() } } pub fn redact(&self, value: &str) -> String { self.secrets.iter().fold(value.to_owned(), |text, secret| text.replace(secret, "[REDACTED]")) } }
/// Assigns deterministic host stream sequence numbers and one conservative outcome.
#[derive(Default)] pub struct StreamNormalizer { next: u64 }
impl StreamNormalizer { pub fn event(&mut self, raw: &str, redactor: &Redactor) -> (u64, String) { self.next = self.next.saturating_add(1); (self.next, redactor.redact(raw)) } pub fn outcome(&self, invocation_id: StableId, source: Option<bool>) -> CapabilityOutcome { let kind = match source { Some(true) => OutcomeKind::Succeeded, Some(false) => OutcomeKind::FailedSafe, None => OutcomeKind::UnknownEffect }; CapabilityOutcome { invocation_id, retry_safe: matches!(kind, OutcomeKind::FailedSafe), kind } } }
