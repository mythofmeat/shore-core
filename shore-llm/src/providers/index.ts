import type { ServerResponse } from "node:http";
import type { Provider, ProviderRequest, NormalizedResponse } from "./types.js";
import * as anthropic from "./anthropic.js";
import * as deepseek from "./deepseek.js";
import * as gemini from "./gemini.js";
import * as openai from "./openai.js";
import * as openrouter from "./openrouter.js";
import * as zhipuai from "./zhipuai.js";

export type { ProviderRequest, NormalizedResponse, Provider };
export type {
  StreamEvent,
  NormalizedUsage,
  NormalizedTiming,
  NormalizedContentBlock,
  EmbedRequest,
  EmbedResponse,
  ImageGenerateRequest,
  ImageGenerateResponse,
} from "./types.js";

export function getProvider(name: string): Provider | null {
  switch (name) {
    case "anthropic":
      return {
        async generate(req: ProviderRequest): Promise<NormalizedResponse> {
          const client = anthropic.createClient(req.api_key, req.base_url);
          return anthropic.generate(
            client,
            req as unknown as anthropic.GenerateRequest,
          );
        },
        async stream(
          req: ProviderRequest,
          res: ServerResponse,
        ): Promise<void> {
          const client = anthropic.createClient(req.api_key, req.base_url);
          return anthropic.stream(
            client,
            req as unknown as anthropic.GenerateRequest,
            res,
          );
        },
      };

    case "openai":
    case "xai":
      return {
        async generate(req: ProviderRequest): Promise<NormalizedResponse> {
          const client = openai.createClient(req.api_key, req.base_url);
          return openai.generate(client, req, name);
        },
        async stream(
          req: ProviderRequest,
          res: ServerResponse,
        ): Promise<void> {
          const client = openai.createClient(req.api_key, req.base_url);
          return openai.stream(client, req, res, name);
        },
      };

    case "deepseek":
      return {
        generate: (req) => deepseek.generate(req),
        stream: (req, res) => deepseek.stream(req, res),
      };

    case "openrouter":
      return {
        generate: (req) => openrouter.generate(req),
        stream: (req, res) => openrouter.stream(req, res),
      };

    case "zhipuai":
      return {
        generate: (req) => zhipuai.generate(req),
        stream: (req, res) => zhipuai.stream(req, res),
      };

    case "gemini":
      return {
        async generate(req: ProviderRequest): Promise<NormalizedResponse> {
          const client = gemini.createClient(req.api_key);
          return gemini.generate(client, req);
        },
        async stream(
          req: ProviderRequest,
          res: ServerResponse,
        ): Promise<void> {
          const client = gemini.createClient(req.api_key);
          return gemini.stream(client, req, res);
        },
      };

    default:
      return null;
  }
}
