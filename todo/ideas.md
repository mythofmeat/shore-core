# Beets integration
- character can see your music library
- query my music library to find what i've listened to, when, and what i've rated it, how much i've listened to it, etc.

# Group Chats
- what if characters could choose to message another character as a part of their potential options on what to do when they receive a heartbeat prompt
- could create groups of characters that are allowed to interact with one another. and have the messages work kind of like an inbox/outbox text message sort of system, where the response from the other character would happen during *their* heartbeat (because the heartbeat probe told them they had an unanswered message from the other character)
- important: let's not go overboard. this is a small flavor thing. we don't want runaway costs here. but a couple cents every couple of days is a reasonable price to pay for a more alive-seeming companion.
- the user can check in on the group chats and send messages in the group chat that can be optionally responded to when the character's heartbeat probe comes in

# Research: Video Input for AI Companions (March 2026)

## Provider Support

**Claude (Anthropic):** No video input. Text + images only. No announced plans.

**OpenAI:** Realtime API does audio only. Video input via API not publicly available.

**Gemini (Google): The only real option.** The Live API accepts real-time video
streams over WebSocket. Video is tokenized at 258 tokens/second (1 fps sampling).

## Cost Math

Video tokenization: 258 tokens/sec = ~15,480 tokens/min = ~929K tokens/hour

### Standard Gemini API (upload video clips)

Gemini 3 Flash at $0.25/1M input tokens:
- ~$0.23/hour of video input — very cheap

### Gemini Live API (real-time streaming)

$3.00/1M tokens for video input, BUT: Live API session billing is calculated
per turn for ALL tokens in the session context window, including accumulated
tokens from previous turns. Tokens are re-billed every turn.

If the character comments every minute over a 1-hour session (60 turns):
- Total billed input: ~28M tokens (triangular accumulation)
- Cost: ~$85/hour just for video input

Commenting every 5 minutes (12 turns/hour): ~$6/hour.

### Hybrid approach (cheapest practical option)

Skip the Live API. Use OBS WebSocket to grab a clip every N minutes, upload as
a video file to the standard Gemini API, get the character's reaction, route it
through Shore.

- 10-second clip every 2 minutes = ~5 min of video/hour
- At $0.25/1M tokens: ~$0.02/hour

## Summary

| Approach                                 | Possible? | Cost/hour |
|------------------------------------------|-----------|-----------|
| Gemini Live API (true real-time stream)  | Yes       | $6-85     |
| Gemini standard API (periodic clips)     | Yes       | $0.02-0.23 |
| Claude / OpenAI                          | No        | N/A       |

## Conclusion

True real-time video streaming is possible via Gemini Live API, but per-turn
re-billing makes it expensive for a chatty companion. The practical move is
periodic clip capture from OBS — the character "watches" 10-30 second clips
every few minutes and reacts. Not continuous vision, but close enough to feel
like watching together, and costs almost nothing.

Shore already supports Gemini as a provider, so the plumbing is partially there.

## My verdict
probably not worth it right now. would work much better as a combination of video+audio for like, a screen-shared discord call kind of feeling. deferring until that becomes a possibility. Check back in like... august or something?
