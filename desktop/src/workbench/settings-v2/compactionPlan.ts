/// The compaction plan a model's declared window implies, mirrored from the
/// runtime policy in desktop/src-tauri/src/runtime/compaction/mod.rs.
///
/// It exists so the Settings panel can show what the current policy actually
/// does instead of describing it in prose that drifts. The fixed context - the
/// workspace and per-tool instructions plus the advertised tool schemas - is
/// measured per request at runtime and cannot be known here, so this reports
/// the target occupancy and the split rather than exact token counts.
export const COMPACTION_TARGET_RATIO = 0.25;
export const DEFAULT_SUMMARY_SHARE = 0.382;

export interface CompactionReadout {
  /// The window compaction budgets against: the declared window minus the
  /// provider's own output reservation.
  window: number;
  /// Occupancy one compaction aims to leave behind.
  target: number;
  /// Output tokens the model declares for itself, clamped to the target share.
  reservation: number;
  summaryPercent: number;
  tailPercent: number;
}

export function compactionReadout(
  contextWindow: number | null | undefined,
  maxOutputTokens: number | null | undefined,
  summaryShare: number,
): CompactionReadout | null {
  if (contextWindow == null || !Number.isFinite(contextWindow) || contextWindow <= 0) {
    return null;
  }
  const reservation = Math.min(
    maxOutputTokens ?? 0,
    Math.floor(contextWindow * COMPACTION_TARGET_RATIO),
  );
  const window = contextWindow - reservation;
  const share = Number.isFinite(summaryShare) ? summaryShare : DEFAULT_SUMMARY_SHARE;
  return {
    window,
    target: Math.floor(window * COMPACTION_TARGET_RATIO),
    reservation,
    summaryPercent: Math.round(share * 100),
    tailPercent: Math.round((1 - share) * 100),
  };
}

/// One sentence naming the numbers, plus the two facts that move them.
export function describeCompaction(readout: CompactionReadout | null): string | null {
  if (!readout) return null;
  const sentences = [
    `A ${readout.window.toLocaleString()}-token window targets ${readout.target.toLocaleString()} tokens after compaction: the summary gets ${readout.summaryPercent}% of what is replaced and the most recent messages keep ${readout.tailPercent}%.`,
  ];
  if (readout.reservation > 0) {
    sentences.push(
      `This model reserves ${readout.reservation.toLocaleString()} tokens for its own output, so a summary longer than that is capped and the difference stays verbatim.`,
    );
  }
  sentences.push(
    "Instructions and tool schemas are measured per request and budgeted before both.",
  );
  return sentences.join(" ");
}
