export type ProviderProtocol =
  | "openai_compatible"
  | "anthropic"
  | "gemini";

export interface ProviderPreset {
  readonly id: string;
  readonly name: string;
  readonly protocol: ProviderProtocol;
  readonly baseUrl: string;
  readonly location: "hosted" | "local";
  readonly modelDiscovery: boolean;
  readonly credentialRequired: boolean;
  readonly description: string;
}

/**
 * Aworkit-owned provider presets. They populate an editable draft; they never
 * install, enable, authenticate, or select a model by themselves.
 */
export const PROVIDER_PRESETS: readonly ProviderPreset[] = [
  {
    id: "openai",
    name: "OpenAI",
    protocol: "openai_compatible",
    baseUrl: "https://api.openai.com/v1",
    location: "hosted",
    modelDiscovery: true,
    credentialRequired: true,
    description: "OpenAI API using the Aworkit OpenAI-compatible adapter.",
  },
  {
    id: "anthropic",
    name: "Anthropic",
    protocol: "anthropic",
    baseUrl: "https://api.anthropic.com",
    location: "hosted",
    modelDiscovery: true,
    credentialRequired: true,
    description: "Claude Messages API with native Anthropic request semantics.",
  },
  {
    id: "gemini",
    name: "Google Gemini",
    protocol: "gemini",
    baseUrl: "https://generativelanguage.googleapis.com",
    location: "hosted",
    modelDiscovery: true,
    credentialRequired: true,
    description: "Gemini API with native model discovery and content generation.",
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    protocol: "openai_compatible",
    baseUrl: "https://openrouter.ai/api/v1",
    location: "hosted",
    modelDiscovery: true,
    credentialRequired: true,
    description: "OpenAI-compatible multi-provider routing endpoint.",
  },
  {
    id: "deepseek",
    name: "DeepSeek",
    protocol: "openai_compatible",
    baseUrl: "https://api.deepseek.com/v1",
    location: "hosted",
    modelDiscovery: true,
    credentialRequired: true,
    description: "DeepSeek's OpenAI-compatible endpoint.",
  },
  {
    id: "groq",
    name: "Groq",
    protocol: "openai_compatible",
    baseUrl: "https://api.groq.com/openai/v1",
    location: "hosted",
    modelDiscovery: true,
    credentialRequired: true,
    description: "Groq's OpenAI-compatible endpoint.",
  },
  {
    id: "mistral",
    name: "Mistral",
    protocol: "openai_compatible",
    baseUrl: "https://api.mistral.ai/v1",
    location: "hosted",
    modelDiscovery: true,
    credentialRequired: true,
    description: "Mistral's OpenAI-compatible endpoint.",
  },
  {
    id: "xai",
    name: "xAI",
    protocol: "openai_compatible",
    baseUrl: "https://api.x.ai/v1",
    location: "hosted",
    modelDiscovery: true,
    credentialRequired: true,
    description: "xAI's OpenAI-compatible endpoint.",
  },
  {
    id: "ollama",
    name: "Ollama",
    protocol: "openai_compatible",
    baseUrl: "http://127.0.0.1:11434/v1",
    location: "local",
    modelDiscovery: true,
    credentialRequired: false,
    description: "Local Ollama service through its OpenAI-compatible endpoint.",
  },
  {
    id: "lm_studio",
    name: "LM Studio",
    protocol: "openai_compatible",
    baseUrl: "http://127.0.0.1:1234/v1",
    location: "local",
    modelDiscovery: true,
    credentialRequired: false,
    description: "Local LM Studio OpenAI-compatible server.",
  },
  {
    id: "custom_openai",
    name: "Custom OpenAI-compatible",
    protocol: "openai_compatible",
    baseUrl: "",
    location: "hosted",
    modelDiscovery: true,
    credentialRequired: false,
    description: "An editable HTTP(S) endpoint using OpenAI-compatible JSON.",
  },
] as const;

export function providerPreset(id: string): ProviderPreset | undefined {
  return PROVIDER_PRESETS.find((preset) => preset.id === id);
}
