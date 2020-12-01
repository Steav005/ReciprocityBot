![Rust](https://github.com/Steav005/ReciprocityBot/workflows/Rust/badge.svg) [![Docker](https://img.shields.io/docker/v/autumnal/reciprocity_bot?color=blue&label=Docker&sort=semver)](https://hub.docker.com/repository/docker/autumnal/reciprocity_bot)
# Reciprocity Bot

## Features

- [ ] Task Scheduler
    - [x] Change Channel ID on the Fly
    - [x] Receive and Distribute Tasks
    - [x] Add All Necessary Tasks 
    - [ ] Log (info, debug, warn, error)
    - [x] Comment
- [ ] Guild Handler
    - [ ] Message Manager
    - [ ] Incoming Event Pipeline
    - [ ] Outgoing Task Scheduler
    - [ ] Voice Handler
    - [ ] Periodical Check for potentially missed events
    - [ ] Log (info, debug, warn, error)
    - [ ] Comment
- [ ] Event Handler
    - [x] Ignore Duplicates
    - [x] Pass Non-Duplicates to GuildHandler
    - [ ] Rethink EventHandler Config
    - [x] Log
    - [x] Comment
- [ ] Message Manager
    - [ ] Manage all Bot Messages
    - [ ] Log (info, debug, warn, error)
    - [ ] Comment
- [ ] Voice Handler
    - [ ] Manage Voice Connection
    - [ ] Enqueue Songs
    - [ ] Log (info, debug, warn, error)
    - [ ] Comment
- [x] Moved to Smol