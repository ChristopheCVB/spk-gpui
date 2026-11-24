# Use a pinned SPK Web instance for desktop smoke tests

Run the Linux desktop client against an isolated SPK Web instance. The real server lets the smoke test exercise sign-in, message history, pins, and Webhooks together without maintaining another implementation of the SPK protocol in the fixture.

## Upstream revisions

- **PR CI and local reproduction.** The smoke test uses the same pinned upstream revision in both environments so the result is reproducible.
- **Local feature development.** Fetch the current upstream `main` when starting Web parity work. Record its commit SHA with the verification evidence and use that snapshot for reruns.

## First smoke test

Seed multiple text and voice channels and message history with a Webhook post, pinned messages, formatted Markdown, links, mentions, and a spoiler containing a Markdown reference link to another text channel. Verify that the channels and seeded content appear, open the pinned messages list, then reveal the spoiler and follow its link. Assert that the referenced channel opens inside the desktop app and that no browser launch was attempted. Existing LiveKit tests cover joining voice and media playback.

## Test environment

Run the Linux desktop binary on Linux CI and from WSL. Route audio to a silent output and intercept browser launches. Stop the fixture's processes on exit and save failure logs and screenshots separately from disposable files.

For local runs, keep shared temporary files through all checks, retries, and screenshot capture. Clean up the recorded paths once the work is complete. For PR work, do this after attaching the requested evidence and verifying the merge. In CI, the runner discards its temporary files at the end of the job.
