swift: `check` runs `swift test` and lists failing tests from XCTest and Swift Testing with their messages.
`check "build"` runs `swift build`. There is no default lint task; pass one with `check "lint" cmd:"swiftlint"`.

- One test: `check args:["--filter", "SongTests"]`.
- Xcode projects: `check cmd:"xcodebuild test -scheme App -destination 'platform=macOS'"`; failures are parsed the same way.
- `outline` and `read symbol:` know func, struct, class, enum, protocol, extension and actor.
