so currently the actual utility of the heartbeat, dreaming and compaction in my own personal configuration ([~/Documents/qifei/config/characters/qifei/workspace/]) seems to be very muddled.

I want to know:
1. what is dreaming intended to do.
2. what is the exact prompt that dreaming uses to achieve this?
3. does dreaming break the cache prefix, or is it a prompt appended to the chat history?
4. what is compaction intended to do?
5. what is the exact prompt that compaction uses?

in my own ideal version of the program...

**dreaming:**
dreaming does more or less what i currently have my heartbeat configured to be: to look through recently updated files, and to distill information, update old info, re-organize files, etc.

**compaction:**
updates the in-prompt `MEMORY.md` file to carry over the context of the previous conversation(s) so that compacted conversations can more or less pick up where they left off

**heartbeat:**
for character-guided autonomous actions. this is very open-ended. it can *include* some further reorganization, but that shouldn't be the point for every character.

**MEMORY.md:**
an index of relevant files, as well as the ongoing conversational context.

does any of this make sense?

---

ADDITIONALLY: i don't think that *anything* is currently giving the character information about how `<sendMessage>` works. at all. or that their response will not be persisted/shown to the user. that info should go into the default heartbeat prompt somewhere, i think.
