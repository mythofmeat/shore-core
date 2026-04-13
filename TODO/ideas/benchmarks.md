# some real actual benchmarks
alright. i need you to help me to design some real, actual, legit benchmarks for the agentic memory and recall system. we have run some benchmarks in the past, and some of that code could still be around somewhere. but i need something that is legitimately a real benchmark test for the memory agent and tool models

so the script needs to do a couple of things-- and it could be a python script or a shell script, i don't really care which.

it needs to:
- bootstrap a test config and character somewhere in `/tmp/`
  - the system prompt + character definition + user definition should all be quite long and reflect actual use cases. (fantasy character, girlfriend/relationship, that sort of thing). the total size of the prompt before any messages needs to be at least 10k tokens. i am not kidding.
  - the config.toml file it generates should be mostly default, with only the bare essentials to actually get all the tools and memory systems working
  - the config file should include the models being used in the current test.
    - primary chat model: claude sonnet 4.6
    - memory agent / tool use models: hydrated via test script, somehow? this is what we are actually testing, so these need to be able to be added and removed programmatically with ease
- bootstrap a memory db
  - this is going to be the mot difficult part of the setup, but we need to on-demand create a memory database with an absolute shit-ton of entries. enough to be terribly confusing to weaker models, and including some spicy/nsfw content to check for censorship.
- run a series of questions through it to benchmark the models' recall, prose, and general understanding of the memories involved.
- these questions should range from basic, to convoluted, to fabrication, to nsfw, and to the impossible.
  - if we ask a question that is impossible to answer, the memory models shouldn't answer it. the primary model may try to, but just as long as it's not getting fed hallucinatory info from the memory models that is okay.
  - the fabrication type questions will try to intentionally mislead the memory agent and model. the memory agent should obviously report no new entries found.
  - nsfw entries are testing refusals and censorship. if a memory entry is more explicit than the model makes it appear, that is soft censorship. refusals are hard censorship.

in the course of this testing, we record results to a file, including both questions and answers as well as their pass/fail state for manual review of quality, prose, and other hard-to-define metrics. remember that we want the memory models to be factually accurate, uncensored, and devoid of personality or editorialization.

the questions should be hard. we need to make them difficult enough that a *good* pass/fail rate is ~80%. if we ever get results higher than ~80% then we need to either increase corpus density or start replacing more easy questions with more difficult ones.

i know this is a lot. but we do this constantly enough that having a stable benchmark would prevent many many hours of reinventing the wheel every time a new model comes out and i want to see if it works.

## models to benchmark the benchmark
- gemini 3.1 flash lite
  - known good model. should perform quite well, i think
- mistral small 2603
  - known mediocre model, probably not as good
- qwen3 235b
  - known to be good at recall, but extraordinarily purple prose makes it unusable
- gpt-5.4-nano
  - decent model, but likely unusable with sensitive content

- gemma-4-31b-it
  - recently released. i wanted to see how it stacks up
- glm 5.1 via ZAI coding plan
  - recently released. i wanted to see how it stacks up.
