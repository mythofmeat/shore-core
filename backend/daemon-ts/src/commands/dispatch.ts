import { usagePayload } from "../ledger/usage.ts";
import {
  asArgs,
  CommandError,
  engineRequired,
  mapUnknownError,
  type DispatchInput,
} from "./types.ts";
import {
  characterInfo,
  listCharacters,
  switchCharacter,
} from "./navigation.ts";
import {
  deleteMessages,
  editMessage,
  getMessage,
  historyPage,
  injectSystem,
  listAlternatives,
  log,
  selectAlternative,
} from "./conversation.ts";
import {
  compact,
  config,
  configCheck,
  configReset,
  diagnostics,
  heartbeatLog,
  heartbeatSetActive,
  heartbeatSetDormant,
  heartbeatTickNow,
  listModels,
  memory,
  memoryChangelog,
  memoryDream,
  memoryDreams,
  modelInfo,
  modelSettings,
  resetModel,
  setModelSetting,
  status,
  switchModel,
} from "./state.ts";
import {
  listProviderModels,
  listProviders,
  refreshAllProviderModels,
  refreshProviderModels,
} from "./providers.ts";

export async function dispatchCommand(input: DispatchInput): Promise<unknown> {
  const { ctx, name, args } = input;
  try {
    switch (name) {
      // Navigation
      case "list_characters":
        return listCharacters(ctx, input.engine);
      case "switch_character":
        return switchCharacter(ctx, engineRequired(input.engine, name), args);
      case "character_info":
        return characterInfo(ctx, engineRequired(input.engine, name), args);

      // Conversation
      case "log":
        return log(engineRequired(input.engine, name), args);
      case "history_page":
        return historyPage(engineRequired(input.engine, name), args);
      case "get":
        return getMessage(engineRequired(input.engine, name), args);
      case "edit":
        return editMessage(engineRequired(input.engine, name), args);
      case "delete":
        return deleteMessages(engineRequired(input.engine, name), args);
      case "alt":
        return selectAlternative(engineRequired(input.engine, name), args);
      case "list_alternatives":
        return listAlternatives(engineRequired(input.engine, name), args);
      case "inject_system":
      case "inject_system_message":
        return injectSystem(engineRequired(input.engine, name), args, rfc3339LocalNow);

      // State
      case "status":
        return status(ctx, engineRequired(input.engine, name));
      case "list_models":
        return listModels(ctx, args);
      case "model_info":
        return modelInfo(ctx, args);
      case "switch_model":
        return switchModel(ctx, args);
      case "reset_model":
        return resetModel(ctx);
      case "set_model_setting":
        return setModelSetting(ctx, args);
      case "model_settings":
        return modelSettings(ctx, args);
      case "memory_changelog":
        return memoryChangelog(ctx, engineRequired(input.engine, name), args);
      case "memory_dream":
        return memoryDream(ctx, engineRequired(input.engine, name), args);
      case "memory_dreams":
        return memoryDreams(ctx, engineRequired(input.engine, name), args);
      case "memory":
        return memory(ctx, engineRequired(input.engine, name), args);
      case "compact":
        return compact(ctx, engineRequired(input.engine, name), args);
      case "config":
        return config(ctx, args);
      case "config_check":
        return configCheck(ctx);
      case "config_reset":
        return configReset(ctx);
      case "diagnostics":
        return diagnostics(ctx, args);
      case "heartbeat_log":
        return heartbeatLog(ctx, engineRequired(input.engine, name), args);
      case "heartbeat_tick_now":
        return heartbeatTickNow(ctx, engineRequired(input.engine, name));
      case "heartbeat_set_dormant":
        return heartbeatSetDormant(ctx, engineRequired(input.engine, name));
      case "heartbeat_set_active":
        return heartbeatSetActive(ctx, engineRequired(input.engine, name));
      case "usage":
        return usagePayload(ctx.ledger, asArgs(args), ctx.runtime.config.app.usage, ctx.pricing);

      // Providers
      case "list_providers":
        return listProviders(ctx);
      case "refresh_provider_models":
        return refreshProviderModels(ctx, args);
      case "refresh_all_provider_models":
        return refreshAllProviderModels(ctx);
      case "list_provider_models":
        return listProviderModels(ctx, args);

      default:
        throw new CommandError("invalid_request", `Unknown command: ${name}`);
    }
  } catch (e) {
    mapUnknownError(e);
  }
}

function rfc3339LocalNow(): string {
  const now = new Date();
  const tzOffsetMinutes = -now.getTimezoneOffset();
  const sign = tzOffsetMinutes >= 0 ? "+" : "-";
  const abs = Math.abs(tzOffsetMinutes);
  const tzh = String(Math.floor(abs / 60)).padStart(2, "0");
  const tzm = String(abs % 60).padStart(2, "0");
  const pad = (n: number, w = 2) => String(n).padStart(w, "0");
  const ms = String(now.getMilliseconds()).padStart(3, "0");
  return (
    `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}` +
    `T${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}.${ms}${sign}${tzh}:${tzm}`
  );
}
