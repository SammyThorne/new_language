# Task creation: 10,000 tasks that each add two numbers, joined, twenty times. CPython runs them
# on a pool of 64 threads: starting 200,000 OS threads one by one would measure thread creation.
from concurrent.futures import ThreadPoolExecutor


def add(a: int, b: int) -> int:
    return a + b


def main() -> None:
    total = 0
    with ThreadPoolExecutor(max_workers=64) as pool:
        for round in range(20):
            futures = [pool.submit(add, i, round) for i in range(10000)]
            total += sum(f.result() for f in futures)
    print(total)


main()
