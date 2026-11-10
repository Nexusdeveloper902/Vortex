# Project Specification — Proton-Based Linux Gaming Launcher

**Document type:** Product and engineering specification  
**Status:** Initial development specification  
**Platform:** Linux desktop  
**Primary purpose:** Installing, managing, and launching Windows games through Proton outside Steam.

---

# 1. Product vision

Build a modern, native Linux desktop application that makes installing and running Windows games through Proton straightforward, reliable, and largely automated.

The application should allow users to import Windows game installers, create isolated Proton environments, install games and their prerequisites, register installed executables, configure compatibility settings, and launch games without manually managing Wine prefixes, Proton commands, dependencies, or compatibility configurations.

The application should act as a dedicated gaming launcher and environment manager, combining the convenience of a traditional game library with the compatibility capabilities of Proton.

It must use existing compatibility technologies rather than reimplementing them.

## Core principles

1. **Proton-first:** Proton is the primary compatibility runtime.
2. **UMU-backed:** Use UMU Launcher to execute Proton outside Steam.
3. **Gaming-focused:** Optimize the product around Windows game installation and execution.
4. **Automation-first:** Automate routine installation and configuration wherever reliable.
5. **Isolation:** Each game receives its own independent environment by default.
6. **User control:** Users can inspect and modify their environments.
7. **Performance-conscious:** Preserve Proton's gaming-oriented graphics and synchronization optimizations.
8. **Reliability:** Never silently corrupt an existing prefix or destroy user data.
9. **Extensibility:** Allow future integration of community-maintained game installation recipes.
10. **Minimal friction:** Users should not need to understand Proton internals to install and play games.

# 2. Target use cases

The primary user is a Linux desktop gamer who wants to run Windows games without depending on Steam as the installation or library manager.

Supported scenarios include:

- Installing games through standalone Windows EXE installers.
- Installing games through MSI installers.
- Installing games through installers distributed as directories containing multiple files.
- Installing games from third-party marketplaces and legitimate DRM-free distributions.
- Running installers that automatically launch prerequisite installers.
- Installing missing dependencies manually.
- Managing different Proton versions for different games.
- Running additional Windows executables inside existing game environments.
- Launching installed games independently of their original installers.
- Troubleshooting compatibility issues through logs and configuration.

The application must not assume every game has a conventional installation process.

# 3. Runtime and compatibility architecture

## 3.1 Proton execution

Use UMU Launcher as the primary execution interface for Proton outside Steam.

The application must:

- Discover available Proton installations.
- Support GE-Proton.
- Support standard Proton distributions where their use outside Steam is supported.
- Allow per-game Proton version selection.
- Allow users to install and update supported Proton distributions.
- Execute arbitrary Windows executables through the selected Proton environment.
- Preserve runtime configuration between installations and launches.
- Use the appropriate Steam Linux Runtime environment through UMU.

Investigate UMU's current upstream interfaces and requirements before implementation.

Do not independently implement or fork Proton, Wine, DXVK, VKD3D-Proton, or Steam Runtime functionality.

## 3.2 Graphics and performance

Preserve the performance capabilities of Proton.

Support configuration and diagnostics for:

- DXVK.
- VKD3D-Proton.
- Vulkan.
- Esync and Fsync where supported.
- Shader caches.
- Game-specific environment variables.
- Launch arguments.
- Graphics compatibility options.

Do not disable performance optimizations by default without a compatibility reason.

Avoid promising native performance for every Windows game.

The application should detect or report missing graphics prerequisites where possible.

## 3.3 Runtime management

Users should be able to:

- View available Proton versions.
- Install supported Proton distributions.
- Update existing distributions.
- Select a global default.
- Override the runtime for individual games.
- Inspect which runtime is being used by a game.

Avoid unnecessary duplicate downloads and installations.

# 4. Game environments and prefix management

A game environment is the central unit of isolation and configuration.

Each game should receive its own prefix by default.

An environment must contain or reference:

- Wine prefix.
- Selected Proton distribution.
- Game installation directory.
- Registered executable.
- Launch arguments.
- Environment variables.
- Dependency information.
- Installation metadata.
- Relevant logs.

Support:

- Creating environments.
- Opening environment directories.
- Running arbitrary executables inside an environment.
- Running additional installers inside an existing environment.
- Installing dependencies.
- Backing up and restoring environments.
- Deleting environments safely.
- Changing runtime versions.

Users should be able to select custom installation and prefix locations.

## Shared environments

The architecture should not make shared prefixes impossible.

Future support may allow multiple applications to share one environment when required by a particular installation.

# 5. Installation system

The installation system is one of the most important components of the application.

It must support different installer formats, directory structures, prerequisite systems, and installation procedures.

## 5.1 Supported installer sources

Initially support:

- Standalone EXE installers.
- MSI installers.
- Directory-based installers containing an EXE and associated data files.

The application must support both single-file and multi-file installation sources.

## 5.2 Multi-file and directory-based installers

The application MUST support installers distributed as directories containing a primary executable and additional required files.

Examples:

```text
GameInstaller/
├── setup.exe
├── data1.bin
├── data2.bin
├── data3.bin
├── setup-1.bin
├── setup-2.bin
└── Redist/
    ├── vcredist_x64.exe
    └── DirectX/
        └── DXSETUP.exe
```

Other possible accompanying formats include:

- BIN.
- CAB.
- DAT.
- RAR and ZIP archives where supported extraction is implemented.
- Numbered installer data archives.
- Additional resource files.
- Subdirectories containing prerequisite installers.

The application must not assume that an EXE installer is self-contained.

### Source selection

Allow users to select either:

1. A standalone installer file.
2. An entire installer directory.

When a directory is selected, the application should:

- Identify likely installer executables.
- Allow manual selection of the primary executable.
- Preserve the original directory structure.
- Preserve all associated files.
- Recognize common installer naming conventions.
- Avoid automatically executing every executable found in the directory.

If automatic identification is ambiguous, ask the user to select the primary installer.

### Execution and working directory

The launcher must execute the primary installer through UMU and Proton.

It must configure the process working directory correctly.

This is essential because many installers locate their data files using relative paths.

The installer must be able to access associated files using the paths it expects.

The implementation must distinguish between:

- The Windows executable path.
- The Windows working directory.
- The host filesystem path.
- The Proton prefix path.

Do not assume these paths are interchangeable.

### Installer staging

Avoid copying entire installer directories into the Proton prefix unnecessarily.

Large installers can occupy tens or hundreds of gigabytes.

Prefer accessing the original installer directory directly, provided Proton can access it correctly.

If staging is required for compatibility:

- Preserve directory structure.
- Preserve filenames.
- Ensure all referenced files remain accessible.
- Avoid unnecessary duplication.
- Allow the user to select an alternative staging location.

The original installer source must remain untouched.

### Source lifecycle

Retain the installer source reference while installation is in progress.

The user should be able to archive or delete original installer files independently of the installed game after installation completes.

If the source directory becomes unavailable during installation, report the failure rather than silently proceeding with missing resources.

## 5.3 Installation workflow

The installation process should:

1. Receive the installer file or directory.
2. Identify or request the primary installer executable.
3. Allow the user to specify the game name.
4. Allow the user to choose the installation location.
5. Create a dedicated game environment.
6. Select the Proton runtime.
7. Initialize the prefix through the supported runtime mechanism.
8. Launch the installer through UMU.
9. Allow the installer to execute normally.
10. Allow nested prerequisite installers to execute.
11. Preserve all changes inside the prefix.
12. Identify possible installed game executables.
13. Allow the user to confirm the actual game executable.
14. Save game configuration and metadata.
15. Register the game in the library.

Installation must be observable.

Display:

- Current installation status.
- Running installer information.
- Relevant process activity.
- Installation logs.
- Errors and completion status.

Do not falsely indicate completion when child installers are still running.

## 5.4 Nested installers and prerequisites

The application MUST support installers that launch additional installers or prerequisite setup programs.

Examples include:

- Microsoft Visual C++ Redistributables.
- DirectX End-User Runtime.
- .NET Framework.
- PhysX.
- XAudio components.
- Game-specific prerequisite packages.

These installers must execute within the same prefix as the game.

Do not terminate, replace, or interfere with prerequisite installers simply because they are separate processes.

The process manager must account for:

- Child processes.
- Installer subprocesses.
- Asynchronous setup operations.
- Processes that detach or reparent themselves.
- Graphical installer dialogs requiring user interaction.

Do not assume that the initial installer process exiting means the entire installation has finished.

Implement a reliable completion strategy and communicate uncertain process states to the user.

## 5.5 Additional executable execution

Provide a general-purpose operation for running arbitrary EXE and MSI files inside an existing game environment.

Examples:

- Installing a missing runtime.
- Running a game launcher.
- Running a configuration utility.
- Installing game updates.
- Running the game's own uninstaller.
- Testing compatibility with an additional executable.

All operations must use the selected prefix and compatible Proton runtime.

# 6. Dependency management

Dependency management should complement the installer's own prerequisite system rather than replace it.

Support integration with Winetricks for commonly required components.

Users should be able to:

- Install additional dependencies.
- Run dependency installers manually.
- Inspect dependency-related logs.
- Receive recommendations for common missing components.
- Manage dependencies within the correct prefix.

The application must not blindly reinstall dependencies already handled by the game installer.

Dependency handling must be extensible because different games require different prerequisites.

# 7. Game library

Provide a graphical library containing registered games.

Each entry should support:

- Game name.
- Artwork.
- Installation status.
- Installation location.
- Selected executable.
- Proton version.
- Prefix location.
- Launch arguments.
- Environment variables.
- Last launch information.

Core actions:

- Play.
- Stop.
- Configure.
- Run another executable.
- Run installer or update.
- Manage dependencies.
- Open installation directory.
- Open prefix directory.
- View logs.
- Back up.
- Uninstall.

Support searching and basic sorting.

Artwork and metadata retrieval should be optional.

The application must remain functional offline.

# 8. Game launching

Launching a game must reuse its saved environment configuration.

The launch system must:

- Resolve the correct Proton version.
- Use the correct prefix.
- Execute the registered Windows executable.
- Apply configured environment variables.
- Apply launch arguments.
- Capture standard output and error streams where available.
- Track the running game process.
- Allow the user to terminate a running game.

Games must launch directly from the graphical library without requiring terminal interaction.

Support standard Linux desktop integration through XDG desktop entries where appropriate.

# 9. Configuration and advanced controls

Expose simple settings for ordinary users and advanced controls for experienced users.

Per-game settings should include:

- Proton version.
- Prefix location.
- Executable.
- Launch arguments.
- Environment variables.
- Runtime options.
- Dependency management.
- Graphics compatibility options.

Provide global defaults that can be overridden per game.

Avoid forcing users to configure every technical detail during installation.

# 10. Diagnostics and troubleshooting

Provide meaningful diagnostics instead of exposing only raw terminal output.

Support:

- Installation logs.
- Launch logs.
- Proton output.
- Runtime initialization errors.
- Prefix-related errors.
- Dependency-related information.
- Basic Vulkan and graphics diagnostics.

Allow exporting logs.

Where possible, distinguish between:

- Installer failure.
- Runtime initialization failure.
- Missing dependency.
- Missing executable.
- Application crash.
- Unsupported compatibility requirement.
- Missing installer data file.
- Incorrect installer working directory.

Do not claim to automatically diagnose problems that cannot be reliably identified.

# 11. Uninstallation and data management

Support two distinct operations.

**Application uninstallation**

Run the game's own uninstaller inside its existing environment when available.

**Complete environment removal**

Remove the managed game installation and its associated prefix.

Before deletion, clearly inform users which files will be removed.

Never delete arbitrary user directories or unrelated prefixes.

Backups must remain independent of the original environment.

# 12. Security and reliability

Treat imported installers and community-provided configurations as untrusted input.

Requirements:

- Never execute installers as root.
- Never require root privileges for normal game installation or execution.
- Never execute arbitrary community shell scripts automatically.
- Validate paths and prevent directory traversal.
- Avoid destructive operations without explicit confirmation.
- Do not expose secrets or sensitive environment variables in logs.
- Keep game environments isolated.
- Do not modify unrelated system Wine configurations or user data.
- Do not attempt to bypass DRM, licensing restrictions, or anti-cheat protections.
- Validate file access and process execution boundaries.

Report compatibility limitations honestly.

# 13. User interface and experience

The application should feel like a modern native Linux desktop application.

Priorities:

- Clean game library.
- Straightforward installation wizard.
- Directory and file selection.
- Clear installation progress.
- Minimal configuration for normal use.
- Accessible advanced settings.
- Useful error messages.
- Consistent navigation.
- Responsive interface.

The interface should not resemble a collection of terminal commands wrapped in buttons.

# 14. Technical architecture

The implementation is intentionally not tied to a predetermined programming language or GUI framework.

The autonomous development agent should investigate suitable technologies and propose an architecture based on:

- Native Linux desktop integration.
- Reliable process management.
- Asynchronous operations.
- Maintainable separation of responsibilities.
- Good filesystem and process isolation.
- Efficient handling of long-running installer processes.
- Persistent application metadata.
- Straightforward packaging and distribution.

Use existing upstream projects wherever they provide the required functionality.

UMU must remain an execution dependency rather than a forked or reimplemented subsystem.

Use standard Linux directory conventions, including XDG locations.

The first supported environment should be Arch Linux, with compatibility with other mainstream distributions considered in the architecture.

Wayland compatibility is required.

Support AMD, NVIDIA, and Intel graphics hardware where the underlying Proton stack supports it.

# 15. Data persistence

Persist game and environment metadata across application restarts.

The storage system should support:

- Game records.
- Environment records.
- Runtime selections.
- Installation metadata.
- Installer source references.
- Executable paths.
- Launch configuration.
- Dependency records.
- Backup metadata.

Actual game files and Proton prefixes should remain separate from metadata storage.

Support recovery from interrupted installations and unexpected shutdowns.

Do not mark incomplete installations as successful.

# 16. Future extensibility

The architecture should allow future features without requiring a fundamental redesign.

Potential features:

- Community game installation recipes.
- Automatic executable detection.
- Compatibility recommendations.
- Game-specific Proton configurations.
- Community compatibility reports.
- Multiple executable profiles per game.
- Game artwork and metadata providers.
- Gamepad and controller configuration.
- Prefix cloning.
- Cloud backup.
- Game launcher integrations.
- Importing existing Lutris or Bottles environments where technically feasible.
- Installer directory analysis and automatic installer detection.

Community recipes should be declarative, versioned, inspectable, and validated before execution.

They must not have unrestricted access to the host system.

# 17. MVP scope

The initial release must prioritize a fully working installation-to-launch workflow.

Required features:

1. Proton runtime discovery.
2. UMU integration.
3. GE-Proton support.
4. Per-game prefix creation.
5. EXE installer execution.
6. MSI installer execution.
7. Multi-file installer directory support.
8. Correct installer working-directory handling.
9. Nested prerequisite installer support.
10. Arbitrary EXE and MSI execution inside existing prefixes.
11. Manual executable registration.
12. Persistent game library.
13. Game launching with saved configuration.
14. Basic Winetricks integration.
15. Basic installation and launch logs.
16. Environment removal.
17. Native desktop integration.

Automatic metadata scraping, community recipes, cloud synchronization, and advanced gamepad configuration are not MVP blockers.

# 18. Acceptance criteria

The MVP is considered functional only when the following can be demonstrated.

## Installation

- A user can import an EXE installer.
- A user can import an MSI installer.
- A user can select an installer directory containing an EXE and multiple BIN files.
- The launcher identifies or allows selection of the primary installer.
- The original installer directory structure is preserved.
- Relative paths and accompanying data files remain accessible.
- The launcher creates a dedicated prefix.
- The installer launches through UMU and Proton.
- A prerequisite installer launched by the main installer can complete.
- The launcher does not prematurely terminate child processes.
- The game installation completes successfully.

## Environment management

- The resulting prefix persists.
- Additional executables can be run in that prefix.
- Dependencies can be installed without affecting unrelated games.
- Different games can use different Proton versions.
- Prefix backups can be created and restored.

## Launching

- A user can register an installed game executable.
- The game launches from the graphical library.
- The same prefix and runtime configuration are reused.
- Launch arguments and environment variables are respected.

## Persistence and safety

- Games remain registered after restarting the application.
- Interrupted installations do not silently appear successful.
- Uninstalling one game does not damage another game's prefix.
- Original installer sources are not destroyed.
- Destructive operations require appropriate confirmation.

## Diagnostics

- Failed launches produce accessible diagnostic output.
- Runtime and installation errors are distinguishable where possible.
- Missing installer resources and incorrect working directories can be diagnosed where possible.

# 19. Autonomous development agent instructions

You are responsible for investigating, designing, implementing, testing, and documenting this product.

Do not begin by generating the entire application in one pass.

## Phase 0: Investigation and architecture

First:

1. Inspect the existing repository and development environment.
2. Determine whether an existing project structure or implementation should be preserved.
3. Investigate UMU, Proton, GE-Proton, Winetricks, and their upstream interfaces.
4. Investigate process lifecycle and child-process management for Windows installers under Proton.
5. Investigate multi-file installer execution and working-directory handling.
6. Identify technical constraints and potential incompatibilities.
7. Propose an architecture with clear module boundaries.
8. Identify the highest-risk technical assumptions.
9. Produce an implementation roadmap with testable milestones.

Do not start major implementation before producing this proposal.

## Implementation

After architecture approval, implement incrementally.

For every milestone:

- Implement the functionality.
- Add appropriate automated tests.
- Perform real integration tests where possible.
- Verify behavior on an actual Linux environment.
- Document dependencies and setup requirements.
- Report limitations and unresolved issues.

Do not fabricate successful integration tests when the required runtime or hardware is unavailable.

Prefer simple, maintainable implementations over unnecessary abstraction.

Do not reimplement existing compatibility technologies.

Do not sacrifice functionality for superficial UI completeness.

Do not silently substitute ordinary Wine execution for required Proton execution.

Do not declare the project complete until the installation-to-launch workflow has been demonstrated, including a multi-file installer scenario.

## Final deliverables

- Working application.
- Reproducible development setup.
- Build and packaging instructions.
- Automated tests.
- Architecture documentation.
- User documentation.
- Known limitations.
- Clear report of implemented versus outstanding requirements.

**Primary success criterion: A user can select a standalone Windows game installer or a directory containing a multi-file installer, install the game and its prerequisites, register the game executable, and launch the game from the application's library through Proton without manually managing the underlying Wine environment.**
