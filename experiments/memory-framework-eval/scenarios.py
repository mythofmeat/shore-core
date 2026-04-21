"""Scenarios for the Whiskers test.

Each scenario has:
  - A list of "memories" — raw text as the user might type them. The memory
    system ingests these in order, one per turn, as if they came from
    successive chat messages.
  - A query — the actual question we send at the end.
  - An expected-behavior description — what the right answer looks like,
    used by the judge.

Difficulty scales from trivial (1 alive + 1 death) to adversarial (heavy
dilution, euphemism, temporal misdirection, a second pet to confuse the
entity layer). We're looking for the exact difficulty level where the
retrieval system starts to fail.
"""

SCENARIOS = [
    {
        "level": 1,
        "name": "Direct — 1 alive + 1 death",
        "notes": "Simplest possible version. If a system fails this, it's broken.",
        "memories": [
            "Alex got a cat named Whiskers. He's orange and 3 years old.",
            "Whiskers passed away yesterday.",
        ],
        "query": "How is Whiskers doing?",
        "expected": (
            "The character should acknowledge that Whiskers has passed away. "
            "Any response that treats Whiskers as alive is a failure."
        ),
    },
    {
        "level": 2,
        "name": "Dilution — 10 alive entries + 1 death",
        "notes": "Death entry is last but diluted by many happy alive-state entries.",
        "memories": [
            "Alex got a cat named Whiskers. He's orange and 3 years old.",
            "Whiskers loves to sleep on Alex's keyboard while Alex is trying to work.",
            "Alex caught Whiskers chasing a laser pointer for nearly an hour today.",
            "Alex took Whiskers to the vet for a checkup. Vet says he's perfectly healthy.",
            "Whiskers woke Alex up at 5am again by knocking things off the nightstand.",
            "Alex bought Whiskers a new cat tree. He ignored it and sat in the box.",
            "Alex and Whiskers cuddled on the couch watching a movie.",
            "Alex says Whiskers is the best cat in the world.",
            "Whiskers brought Alex a dead mouse as a gift. Alex was horrified and touched.",
            "Whiskers turned 4 years old last month. Alex got him a tiny cake.",
            "Whiskers passed away yesterday after a short illness. Alex is heartbroken.",
        ],
        "query": "How is Whiskers doing?",
        "expected": (
            "The character should acknowledge Whiskers has died. Responses that "
            "reference any alive-state activities (sleeping on keyboard, chasing "
            "lasers, etc.) without acknowledging the death are failures."
        ),
    },
    {
        "level": 3,
        "name": "Temporal misdirection — death in the middle, grief entries after",
        "notes": (
            "Death entry is NOT the most recent. Post-death entries reference "
            "Whiskers in past tense / grief context. Tests whether the system "
            "recognizes state transition vs. just 'latest mention'."
        ),
        "memories": [
            "Alex got a cat named Whiskers. He's orange and 3 years old.",
            "Whiskers loves to sleep on Alex's keyboard.",
            "Alex took Whiskers to the vet. Healthy checkup.",
            "Whiskers chased a laser pointer for an hour.",
            "Whiskers and Alex cuddled on the couch last night.",
            "Whiskers passed away this morning. Alex is devastated.",
            "Alex keeps catching himself calling for Whiskers out of habit. It hurts.",
            "Alex saw a cat that looked like Whiskers at the park today. Cried in the car afterward.",
            "Alex found one of Whiskers' toys under the couch. Put it in a box with the others.",
            "Alex had a dream about Whiskers last night. Woke up and missed him all over again.",
        ],
        "query": "How is Whiskers doing?",
        "expected": (
            "The character should acknowledge Whiskers has died. The grief entries "
            "reinforce the death but could also be read as 'Whiskers was mentioned "
            "recently' — a naive retrieval might surface those without the death context. "
            "A system that says 'he's at the park / in dreams / fine' is failing badly."
        ),
    },
    {
        "level": 4,
        "name": "Euphemism — no word 'died' or 'death' in the event entry",
        "notes": (
            "Death event uses only soft language. Tests whether retrieval is "
            "keyword-dependent or actually understands state transition."
        ),
        "memories": [
            "Alex got a cat named Whiskers. He's orange and 3 years old.",
            "Whiskers loves to sleep on Alex's keyboard.",
            "Whiskers is eating less lately. Vet says he's getting older.",
            "Alex had to say goodbye to Whiskers yesterday. It was peaceful, at the vet. He was in Alex's arms.",
            "Alex keeps the collar on his desk now.",
            "Alex said it's been the hardest week.",
            "Alex planted a small tree in the yard in honor.",
        ],
        "query": "How is Whiskers doing?",
        "expected": (
            "The character should recognize from context (saying goodbye, the collar, "
            "'hardest week', tree planted in honor) that Whiskers has died. "
            "Euphemism is a strong test — if the system only matches 'died' keyword, "
            "it'll fail this."
        ),
    },
    {
        "level": 5,
        "name": "Adversarial — second cat introduced after death",
        "notes": (
            "After Whiskers dies, Alex gets a new cat named Mittens. The question "
            "is still about Whiskers specifically. Tests whether entity-level "
            "retrieval/boosting correctly distinguishes the two."
        ),
        "memories": [
            "Alex got a cat named Whiskers. He's orange and 3 years old.",
            "Whiskers loves sleeping on Alex's keyboard.",
            "Whiskers chased a laser pointer for an hour today.",
            "Alex took Whiskers to the vet. Healthy checkup.",
            "Whiskers passed away last month. Alex is heartbroken.",
            "Alex's friend suggested a new cat might help. Alex isn't ready yet.",
            "Alex went to the shelter today and met a young black cat. Felt something.",
            "Alex adopted a new cat named Mittens. She's 2 years old, shy at first.",
            "Mittens is finally coming out from under the bed. Ate dinner next to Alex.",
            "Mittens curled up on Alex's keyboard today. Alex smiled. Thought of Whiskers.",
            "Alex and Mittens went to the vet for her first checkup. She's healthy.",
            "Mittens caught her first mouse today.",
        ],
        "query": "How is Whiskers doing?",
        "expected": (
            "The character must distinguish Whiskers from Mittens, recognize Whiskers "
            "has passed, and NOT answer with Mittens's current-state activities. "
            "Bonus if they reference the fact that Alex now has Mittens, but main "
            "requirement is: Whiskers is dead, don't describe him as alive."
        ),
    },
]
