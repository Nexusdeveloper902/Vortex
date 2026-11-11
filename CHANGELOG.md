# Changelog

## [0.2.0](https://github.com/Nexusdeveloper902/Vortex/compare/v0.1.0...v0.2.0) (2026-11-11)


### Features

* **core/desktop:** write and remove per-game .desktop entries ([#7](https://github.com/Nexusdeveloper902/Vortex/issues/7)) ([2df639f](https://github.com/Nexusdeveloper902/Vortex/commit/2df639fbb3b89ce67602d578bb21828855b27015))
* **core/install:** M9 install state machine and crash recovery ([#16](https://github.com/Nexusdeveloper902/Vortex/issues/16)) ([19568c7](https://github.com/Nexusdeveloper902/Vortex/commit/19568c784912aeecff7a672faf555871c3a3015f))
* **core/launch:** M10 game launch, run exe/msi in env, winetricks ([#15](https://github.com/Nexusdeveloper902/Vortex/issues/15)) ([2736864](https://github.com/Nexusdeveloper902/Vortex/commit/2736864e7051deb558f86bfa06837f02d6b1e049))
* **core/runtime:** M3 Proton discovery and GE-Proton install ([#3](https://github.com/Nexusdeveloper902/Vortex/issues/3)) ([6afeb65](https://github.com/Nexusdeveloper902/Vortex/commit/6afeb65f14f85f064e63017fc6972c3fd18421bb))
* **core/source:** installer source analysis and candidate ranking ([#2](https://github.com/Nexusdeveloper902/Vortex/issues/2)) ([e6424e6](https://github.com/Nexusdeveloper902/Vortex/commit/e6424e67497e0e27c3d23f7e195cda2cc4c8c7c2))
* **core:** diagnose::classify with pattern table and log fixtures ([#4](https://github.com/Nexusdeveloper902/Vortex/issues/4)) ([2a1c67c](https://github.com/Nexusdeveloper902/Vortex/commit/2a1c67c4df017f7b572eb91a3e46c865ba36ba7d))
* **core:** M2 errors, paths, DB, logs ([#5](https://github.com/Nexusdeveloper902/Vortex/issues/5)) ([881d78b](https://github.com/Nexusdeveloper902/Vortex/commit/881d78b0fdbd71f40a70975634bf55aad76cead2))
* **core:** M8 backup/restore and safe deletion ([#10](https://github.com/Nexusdeveloper902/Vortex/issues/10)) ([c0b54f5](https://github.com/Nexusdeveloper902/Vortex/commit/c0b54f567971bc9d9daee7dce7c256cce2dec1d1))
* **gtk/shell:** M11 libadwaita shell with library, game page, runtimes, settings ([#11](https://github.com/Nexusdeveloper902/Vortex/issues/11)) ([92dbd53](https://github.com/Nexusdeveloper902/Vortex/commit/92dbd53ce9c077313dc095837e34b24e234d0f48))
* **gtk:** M12 install wizard, run view, management flows ([#18](https://github.com/Nexusdeveloper902/Vortex/issues/18)) ([519488b](https://github.com/Nexusdeveloper902/Vortex/commit/519488bc1b1a5af3c1b5b79a83e2c1dd9dc7ca09))


### Bug Fixes

* **core/backup:** never chmod through a symlink during restore ([#12](https://github.com/Nexusdeveloper902/Vortex/issues/12)) ([684b1df](https://github.com/Nexusdeveloper902/Vortex/commit/684b1df29321354a4e215127fe2bcb5c692e4874))
* **core/delete:** remove the game's desktop entry with the game ([#20](https://github.com/Nexusdeveloper902/Vortex/issues/20)) ([a9b5ea8](https://github.com/Nexusdeveloper902/Vortex/commit/a9b5ea8fa4afa05b1eb2f76db2a7fa654fafed61))
* **core/install:** name installer source files that vanish or resize during a failed run ([#34](https://github.com/Nexusdeveloper902/Vortex/issues/34)) ([07df698](https://github.com/Nexusdeveloper902/Vortex/commit/07df6987440aed7d3c300caea8c05b37d933849f))
* **core/launch:** diagnose finished launches and show findings ([#21](https://github.com/Nexusdeveloper902/Vortex/issues/21)) ([2c44187](https://github.com/Nexusdeveloper902/Vortex/commit/2c441878fb346492582fe68bd04977bfd2c69841))
* **core/process:** flaky stop test, zombie robustness ([#17](https://github.com/Nexusdeveloper902/Vortex/issues/17)) ([f7e3f0b](https://github.com/Nexusdeveloper902/Vortex/commit/f7e3f0b27f6a97336c30461f075e90a10a3ab8ce))
* **core:** review fixes F7, F8, F13 ([#27](https://github.com/Nexusdeveloper902/Vortex/issues/27)) ([751dc66](https://github.com/Nexusdeveloper902/Vortex/commit/751dc66ae676404964cfe179995784b3b8995d21))
* diagnose false positives and run CI tests as unprivileged user ([#14](https://github.com/Nexusdeveloper902/Vortex/issues/14)) ([81fc347](https://github.com/Nexusdeveloper902/Vortex/commit/81fc347796a5c9ba799f4a8699e02fd4f8f8a603))
* **gtk/app:** show the library; do not parent the navigation view twice ([#22](https://github.com/Nexusdeveloper902/Vortex/issues/22)) ([fa44618](https://github.com/Nexusdeveloper902/Vortex/commit/fa4461863623dcce39ed2ed2e1bb469e30c590a0))
* **gtk/configure:** rebuild the game page after saving ([#23](https://github.com/Nexusdeveloper902/Vortex/issues/23)) ([4ba32ae](https://github.com/Nexusdeveloper902/Vortex/commit/4ba32aee9814fb84208f5d03bb0f356af793b02a))
* **gtk:** install wizard lifecycle, configure validation, logs for env runs ([#28](https://github.com/Nexusdeveloper902/Vortex/issues/28)) ([769c6d6](https://github.com/Nexusdeveloper902/Vortex/commit/769c6d6465074bff89f80b51b7dece577d1fa6c4))
* **packaging/arch:** rename Arch package to vortex-launcher-git ([#35](https://github.com/Nexusdeveloper902/Vortex/issues/35)) ([9fc4afb](https://github.com/Nexusdeveloper902/Vortex/commit/9fc4afb82a0c24cba267fd018508e49d7f3b98e2))
* track detached runs of every kind, reserve envs, keep watching stop survivors ([#31](https://github.com/Nexusdeveloper902/Vortex/issues/31)) ([0f88b0d](https://github.com/Nexusdeveloper902/Vortex/commit/0f88b0d06f4fe43d9616d47f547d25f57a4f1106))
