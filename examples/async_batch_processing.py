# A slightly richer async example with list processing around awaited calls.
# This matches the style used in Monty's own examples.

async def main():
    urls = [
        "https://example.com/a",
        "https://example.com/b",
        "https://example.com/c",
    ]

    results = []
    for url in urls:
        body = await fetch_text(url)
        results.append(
            {
                "url": url,
                "size": len(body),
            }
        )

    print("processed", len(results), "responses")

    return results


await main()  # pyright: ignore
