This document is meant to concretely write down what *I* as a user want out of this program. This is the ur-text. If you are an AI agent (Claude, ChatGPT/Codex, Gemini, etc.) this is what you want to measure all features and implementation against.

# What `shore` *is*

`shore` is an attempt to be able to make an AI character chat program, improving on some of the pain points I experienced while using SillyTavern, while keeping things distinctly suited to *my* personal aesthetic and functional preferences. 


# What `shore` should be good at:

- Long-running chats with a single character. Multiple characters are technically supported, but in most cases there will be one *main* character persona that the user talks to.
- Giving characters a sense of long-term persistent memory while actual input tokens and context windows small
- Anthropic cache-awareness: Nothing should ever cause a cache invalidation without an obvious cause such as: the conversation has been compacted and the system prompt/character description has been updated. That's it, basically. Any unexpected cache invalidation should be treated as a high-priority critical bug and be dealt with immediately. Cache invalidation wastes actual real-world money. This should be one of the chief concerns when implementing features that impact the message history or system prompt.
- Character autonomy: Characters should be able to reach out unprompted to the user, manage their own files in their workspace for their own projects, memory, and description.
- Giving characters a decent variety of tools to make chats more interesting. Tools are not just for accomplishing goals or solving problems here, they're ways to give the character more interaction and information about the real world, as well as about the user, about themselves, and about `shore`.


# Features

## Long-lived daemon

The daemon should be more-or-less agnostic to what clients are connected. The user should be able to send a quick message from the command line, have a long chat within a TUI client, use a GUI client for a more immersive experience, or send messages through the external connections (matrix or telegram, etc.) to the same character within the same chat.

The inspiration is `mpd`. Having a single music player daemon with many clients that have their own strengths and features but ultimately are simply interacting with the same library/playlist protocol.


## API cost reduction

Anthropic's cache system is unique. `shore` is optimized for Anthropic's 1 hour TTL. Cache reads are priced at 0.10x input cost, and cache writes are priced at 2x input cost. This means that a fully invalidated cache could cause the message to be up to 20 times more expensive. We avoid this by:

- Using cache keepalive for Anthropic models. Sending a brief ping including the full conversation context before the cache expires to keep it warm.
- Smart use of cache breakpoints to reduce cache invalidation costs.
- Ensuring at every step of feature implementation that nothing mutates the system prompt before the conversation state is known to be reset, such as compaction.


## Memory

Keeping context low while providing a long-term memory system similar to openclaw/letta. Allowing the character to update its memories that are simple git-diffable markdown files in a workspace the character has access to. This extends to giving the character the ability to update its *own* system prompt, character/user definitions, and tool descriptions.

### Dreaming

Similar to openclaw's dreaming system, a daily period of self-limited cleanup, updating, curation of memories and files in the workspace.


## Heartbeat/Autonomy

A heartbeat system similar to openclaw that allows the character time to write and update memories, use tools to research subjects, and to optionally send the user a message. The goal is not to make sure that every heartbeat session results in the user receiving an autonomous message, but that the character can decide for themselves what to do with their free time.


## Budget awareness

- Track costs and usage over time
- Set daily budget limits


## Easy model switching

Keep local messages in a format that can be transformed into faithful, best-practice formatting depending on what model's SDK is being utilized. Anthropic, openai-compatible, etc.


## TTS integration

Ability to have messages be read on-demand or automatically via an openai-compatible text-to-speech provider.


## Basic messaging functionality similar to SillyTavern

- Edit existing messages
- Regenerate messages keeping alternate version to choose from
- Delete messages
