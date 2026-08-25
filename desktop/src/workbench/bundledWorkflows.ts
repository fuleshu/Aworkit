import bundledLibrary from "../../workflows/default-workflows.json";
import { parseWorkflow, type WorkflowDocument } from "./workflow";

export interface BundledWorkflowTemplate {
  readonly templateId: string;
  readonly seedOnFreshProfile: boolean;
  readonly workflowId: string;
  readonly name: string;
  readonly document: WorkflowDocument;
}

/** The checked-in JSON bundle is the only source of bundled workflow data. */
export const bundledWorkflowTemplates: readonly BundledWorkflowTemplate[] =
  bundledLibrary.workflows.map((entry) => {
    const document = parseWorkflow(JSON.stringify(entry.document));
    if (typeof document.id !== "string" || typeof document.name !== "string")
      throw new Error(`bundled workflow template '${entry.templateId}' has no id or name`);
    return {
      templateId: entry.templateId,
      seedOnFreshProfile: entry.seedOnFreshProfile,
      workflowId: document.id,
      name: document.name,
      document,
    };
  });

export const bundledDefaultWorkflowId = bundledLibrary.defaultWorkflowId;
export const bundledCreationDefaultTemplateId =
  bundledLibrary.creationDefaultTemplateId;

export const bundledDefaultWorkflow =
  bundledWorkflowTemplates.find(
    ({ workflowId }) => workflowId === bundledDefaultWorkflowId,
  )?.document ?? (() => {
    throw new Error(
      `bundled default workflow '${bundledDefaultWorkflowId}' is missing`,
    );
  })();
