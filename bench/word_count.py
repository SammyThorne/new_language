TEXT = "The language reads like Python and runs like a compiled program. Readability is the first goal, and speed is the second goal. The rest of the plan follows from those two goals: the shape of the syntax, the choice of a collector, the design of traits."

counts: dict[str, int] = {}
for _ in range(500_000):
    for word in TEXT.split():
        key = word.lower()
        counts[key] = counts.get(key, 0) + 1
print(len(counts), counts["the"], counts["goal,"])
