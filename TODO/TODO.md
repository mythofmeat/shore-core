- [ ] can we re-organize this repo in some way? 
  - `shore-server` (or `shore-daemon`, or `shore-backend`, i don't really care): this should contain literally all of the backend code that makes the other programs work
    - this includes the llm client stuff, as well as the configuration stuff
    - this should really just be a long-lived daemon that exposes a REST API that the other stuff can use
    - problems and bugs should be able to be clearly distinguished from frontend concerns vs. backend concerns
    - with the exception of shore-cli, because shore-cli should **be** the same thing as the shore-daemon. having it split was the wrong decision. we should consolidate.
    - shore-server should be a single thing that can be installed and provide a binary that includes the CLI and the daemon. the bare-minimum but still completely usable if you are terminal-centric

  - `shore-clients`: this should be individual clients that can be installed separately and depend on shore-daemon, but communicate via the REST API and otherwise do not need to have any inclusion of backend code or modules
    - `shore-tui`
    - `shore-gui-godot`
    - `shore-gui`

  - `shore-connectors`: optional hooks into other programs for convenience and external. 
    - `shore-matrix`
    - `shore-telegram`

  - `shore-dev`: the testing and development suite
    - `shore-mcp`
    - `shore-test-harness`
    - etc. 
