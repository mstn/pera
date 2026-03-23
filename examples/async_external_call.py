# Example of the external-call shape Monty supports.
# In the current repo this will suspend, dispatch the action, and fail because
# the CLI uses RejectingActionHandler.

async def main():
    response = await fetch_text("https://example.com")

    return {
        "status": "received",
        "response": response,
    }


await main()  # pyright: ignore
