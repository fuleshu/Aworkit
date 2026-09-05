// @vitest-environment jsdom
import { cleanup, render as renderApp, screen, waitFor } from "@testing-library/react";
import { render } from "../test/renderWithNotifications";
import userEvent from "@testing-library/user-event";
import axe from "axe-core";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { defaultDesktopAdapters } from "../adapters/defaultAdapters";
import {
  PreviewManagementRepairCorePort,
  type ManagementRepairCorePort,
} from "./corePort";
import { ManagementRepairScreen } from "./ManagementRepairScreen";
import { createManagementRepairPreviewProjection } from "./preview";
import type {
  ManagementRepairCommandV1,
  ManagementRepairProjectionV1,
  ManagementRepairReceiptV1,
} from "./types";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("Management repair review", () => {
  it("renders the complete accepted candidate disclosure and accessible evidence selection", async () => {
    const { container } = render(
      <ManagementRepairScreen
        corePort={new PreviewManagementRepairCorePort()}
        pollIntervalMs={60_000}
      />,
    );

    expect(
      await screen.findByRole("heading", {
        name: "Bound extension-host restarts after uncertain exit",
      }),
    ).toBeVisible();
    expect(screen.getByText("6 occurrences in 3 Chats")).toBeVisible();
    expect(screen.getByText("Complete source & configuration diff")).toBeVisible();
    expect(
      screen.getByRole("table", {
        name: "Complete source diff for extension_supervisor.rs",
      }),
    ).toHaveAttribute("aria-rowcount", "5");
    expect(screen.getByText("Err(_) => restart_now(id),")).toBeVisible();
    expect(screen.getByText("428 workspace tests passed")).toBeVisible();
    expect(screen.getByText("+1.3%")).toBeVisible();
    expect(screen.getByText("macOS focused restart test")).toBeVisible();
    expect(screen.getByText("0.4.0-dev+g81d1a2f")).toBeVisible();
    expect(screen.getAllByText("No authority broadening.").length).toBeGreaterThan(0);
    expect(screen.getByText("Build origin: Local source build")).toBeVisible();
    expect(
      screen.getByText(
        "Repair activation: Available — unprivileged managed slots",
      ),
    ).toBeVisible();
    expect(
      screen.getByText(
        "Integrity: Same-user hash/ownership; not publisher verified",
      ),
    ).toBeVisible();

    const user = userEvent.setup();
    const filter = screen.getByRole("searchbox", {
      name: "Filter repair evidence",
    });
    await user.type(filter, "capability");
    expect(
      screen.getByRole("button", {
        name: /Managed-local activation capability/,
      }),
    ).toBeVisible();
    await user.click(
      screen.getByRole("button", {
        name: /Managed-local activation capability/,
      }),
    );
    expect(screen.getByText(/ManagedLocalBuildProfileV1 is supported/)).toBeVisible();

    for (const input of container.querySelectorAll("input"))
      expect(input).toHaveAttribute("title", expect.stringMatching(/\S/));
    const results = await axe.run(container, {
      rules: { "color-contrast": { enabled: false } },
    });
    expect(results.violations).toEqual([]);
  });

  it("requires acknowledgement and native confirmation before emitting the exact activation command", async () => {
    const recording = new RecordingPort();
    const confirmDecision = vi.fn(async () => true);
    const user = userEvent.setup();
    render(
      <ManagementRepairScreen
        confirmDecision={confirmDecision}
        corePort={recording}
        pollIntervalMs={60_000}
      />,
    );
    const acknowledgement = await screen.findByRole("checkbox", {
      name: /I reviewed the complete disclosure/,
    });
    const activate = screen.getByRole("button", {
      name: "Activate and restart",
    });
    expect(activate).toBeDisabled();
    await user.click(acknowledgement);
    expect(activate).toBeEnabled();
    await user.click(activate);
    await waitFor(() => expect(recording.commands).toHaveLength(1));
    expect(confirmDecision).toHaveBeenCalledWith(
      "Activate repair R-104 version 3?",
      expect.stringContaining("checkpoint this Management Chat"),
    );
    expect(recording.commands[0]).toEqual({
      type: "activate_repair_and_restart",
      commandId: expect.stringMatching(/^desktop\.management\./),
      candidateId: "R-104",
      expectedCandidateVersion: 3,
      expectedCapabilityDigest: "sha256:capability-generation-18",
    });
    expect(recording.commands[0]).not.toHaveProperty("authority");
    expect(
      await screen.findByText(/Management Chat checkpoint committed/),
    ).toBeVisible();
  });

  it("keeps review and activation bound to an exact duplicate-ID candidate version", async () => {
    const projection = duplicateVersionProjection();
    const port = new RecordingPort(projection);
    const user = userEvent.setup();
    render(
      <ManagementRepairScreen
        confirmDecision={async () => true}
        corePort={port}
        pollIntervalMs={60_000}
      />,
    );

    await user.click(
      await screen.findByTitle("Review repair candidate R-104 version 2"),
    );
    expect(
      screen.getByRole("heading", { name: "Earlier bounded restart candidate" }),
    ).toBeVisible();
    await user.click(
      screen.getByRole("checkbox", {
        name: /I reviewed the complete disclosure/,
      }),
    );
    await user.click(
      screen.getByRole("button", { name: "Activate and restart" }),
    );

    await waitFor(() => expect(port.commands).toHaveLength(1));
    expect(port.commands[0]).toMatchObject({
      type: "activate_repair_and_restart",
      candidateId: "R-104",
      expectedCandidateVersion: 2,
      expectedCapabilityDigest: "sha256:capability-generation-17",
    });
  });

  it.each([
    [
      "EnrollmentRequired",
      "Local build is not enrolled. Set up a managed local installation, then restart from its launcher.",
      "Enable local repair activation",
    ],
    [
      "PackagedDistribution",
      "Self-activation is unavailable for this packaged build. Export the candidate and update manually.",
      "Refresh activation report",
    ],
    [
      "UnknownOrigin",
      "Build origin could not be verified. Self-activation is disabled.",
      "Refresh activation report",
    ],
  ] as const)(
    "presents %s without a force-activate path",
    async (eligibility, exactReason, availableAction) => {
      render(
        <ManagementRepairScreen
          corePort={
            new PreviewManagementRepairCorePort(
              createManagementRepairPreviewProjection({ eligibility }),
            )
          }
          pollIntervalMs={60_000}
        />,
      );
      expect((await screen.findAllByText(exactReason)).length).toBeGreaterThan(0);
      expect(screen.queryByRole("button", { name: "Activate and restart" })).toBeNull();
      expect(
        screen.getByRole("button", { name: availableAction }),
      ).toBeEnabled();
      expect(
        screen.getByRole("button", { name: "Export candidate" }),
      ).toBeEnabled();
      expect(screen.getByText("Diagnosis")).toBeVisible();
    },
  );

  it("freezes every command after an ordered projection gap", async () => {
    const user = userEvent.setup();
    render(
      <ManagementRepairScreen corePort={new GapPort()} pollIntervalMs={10} />,
    );
    const acknowledgement = await screen.findByRole("checkbox", {
      name: /I reviewed the complete disclosure/,
    });
    await user.click(acknowledgement);
    expect(
      await screen.findByText("Management projection disconnected."),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Activate and restart" }),
    ).toBeDisabled();
    expect(screen.getByRole("button", { name: "Export patch" })).toBeDisabled();
  });

  it("keeps unavailable exact evidence explicit and keyboard-blocks activation", async () => {
    const projection = unavailableEvidenceProjection();
    const user = userEvent.setup();
    render(
      <ManagementRepairScreen
        corePort={new PreviewManagementRepairCorePort(projection)}
        pollIntervalMs={60_000}
      />,
    );

    expect(
      await screen.findByText(
        "Activation is blocked until every required artifact is loaded, hash-verified, and parsed into exact review evidence.",
      ),
    ).toBeVisible();
    expect(screen.getByText("Source diff: Unavailable")).toBeVisible();
    expect(screen.getByText("Tests: Unavailable")).toBeVisible();
    expect(
      screen.queryByRole("table", { name: /Complete source diff/ }),
    ).toBeNull();

    const acknowledgement = screen.getByRole("checkbox", {
      name: /I reviewed the complete disclosure/,
    });
    acknowledgement.focus();
    await user.keyboard("[Space]");
    expect(acknowledgement).toBeChecked();
    expect(
      screen.getByRole("button", { name: "Activate and restart" }),
    ).toBeDisabled();
    expect(screen.getByText("The candidate disclosure is incomplete.")).toBeVisible();
  });

  it("reuses the exact command ID after an uncertain native result", async () => {
    const port = new UncertainResultPort();
    const user = userEvent.setup();
    render(
      <ManagementRepairScreen corePort={port} pollIntervalMs={60_000} />,
    );
    const exportPatch = await screen.findByRole("button", {
      name: "Export patch",
    });

    await user.click(exportPatch);
    expect(
      await screen.findByText("native transport result is uncertain"),
    ).toBeVisible();
    await user.click(exportPatch);

    await waitFor(() => expect(port.commands).toHaveLength(2));
    expect(port.commands[1]?.commandId).toBe(port.commands[0]?.commandId);
    expect((await port.snapshot(0)).version).toBe(
      createManagementRepairPreviewProjection().version + 1,
    );
  });

  it("marks Management Chat unsupported in the rescue navigation", () => {
    renderApp(
      <App
        adapters={defaultDesktopAdapters}
        managementRepairCorePort={new PreviewManagementRepairCorePort()}
      />,
    );
    const management = screen.getByRole("button", {
      name: /Management Chat.*Unsupported/,
    });
    expect(management).toBeDisabled();
    expect(management).toHaveAttribute(
      "title",
      "Management Chat is unsupported in this build",
    );
    expect(
      screen.queryByRole("checkbox", {
        name: /I reviewed the complete disclosure/,
      }),
    ).toBeNull();
  });
});

class RecordingPort implements ManagementRepairCorePort {
  public readonly commands: ManagementRepairCommandV1[] = [];
  private readonly delegate: PreviewManagementRepairCorePort;

  public constructor(initial?: ManagementRepairProjectionV1) {
    this.delegate = new PreviewManagementRepairCorePort(initial);
  }

  public snapshot(afterSequence: number): Promise<ManagementRepairProjectionV1> {
    return this.delegate.snapshot(afterSequence);
  }

  public command(
    command: ManagementRepairCommandV1,
    expectedVersion: number,
  ): Promise<ManagementRepairReceiptV1> {
    this.commands.push(command);
    return this.delegate.command(command, expectedVersion);
  }
}

function duplicateVersionProjection(): ManagementRepairProjectionV1 {
  const projection = createManagementRepairPreviewProjection();
  const current = projection.candidates[0];
  const currentReport = projection.capabilityReports[0];
  const older = {
    ...current,
    version: 2,
    title: "Earlier bounded restart candidate",
    candidateHash: "sha256:candidate-r104-v2",
    disclosure: {
      ...current.disclosure,
      hash: "sha256:disclosure-r104-v2",
    },
  };
  return {
    ...projection,
    candidates: [older, current],
    capabilityReports: [
      {
        ...currentReport,
        id: "CAP-R104-V2",
        reportVersion: currentReport.reportVersion - 1,
        candidateVersion: 2,
        candidateHash: older.candidateHash,
        disclosureHash: older.disclosure.hash,
        capabilityGeneration: 17,
        capabilityDigest: "sha256:capability-generation-17",
      },
      currentReport,
    ],
  };
}

function unavailableEvidenceProjection(): ManagementRepairProjectionV1 {
  const projection = createManagementRepairPreviewProjection();
  const candidate = projection.candidates[0];
  return {
    ...projection,
    candidates: [
      {
        ...candidate,
        disclosure: {
          ...candidate.disclosure,
          complete: false,
          sourceDiffEvidence: {
            state: "unavailable",
            explanation:
              "The source artifact reader is unavailable; no lines were projected.",
            artifactIds: ["EV-DIFF-104"],
          },
          sourceDiffs: [],
          configurationDiffEvidence: {
            state: "none_declared",
            explanation: "No configuration changes were declared.",
            artifactIds: [],
          },
          configurationDiffs: [],
          testEvidence: {
            state: "unavailable",
            explanation:
              "The test artifact reader is unavailable; no result status was projected.",
            artifactIds: ["EV-TEST-104"],
          },
          tests: [],
          benchmarkEvidence: {
            state: "not_performed",
            explanation: "Benchmarks were not performed for this candidate.",
            artifactIds: [],
          },
          benchmarks: [],
        },
      },
    ],
  };
}

class GapPort extends PreviewManagementRepairCorePort {
  public override async snapshot(
    afterSequence: number,
  ): Promise<ManagementRepairProjectionV1> {
    if (afterSequence === 0) return await super.snapshot(0);
    const current = await super.snapshot(0);
    return {
      ...current,
      lastSequence: current.lastSequence + 2,
      events: [
        {
          sequence: current.lastSequence + 2,
          kind: "gap",
          occurredAt: "2026-08-21T10:00:00Z",
          subjectId: "R-104",
        },
      ],
    };
  }
}

class UncertainResultPort extends PreviewManagementRepairCorePort {
  public readonly commands: ManagementRepairCommandV1[] = [];
  private loseFirstReceipt = true;

  public override async command(
    command: ManagementRepairCommandV1,
    expectedVersion: number,
  ): Promise<ManagementRepairReceiptV1> {
    this.commands.push(command);
    const receipt = await super.command(command, expectedVersion);
    if (this.loseFirstReceipt) {
      this.loseFirstReceipt = false;
      throw new Error("native transport result is uncertain");
    }
    return receipt;
  }
}
