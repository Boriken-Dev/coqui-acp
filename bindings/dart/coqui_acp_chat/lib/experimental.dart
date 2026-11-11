/// Experimental: what a session hub would need of a chat, which may change or go. `ChatSessionHub` adds `replay`, `applyReplay` and
/// `settleElsewhere` to a `ChatSession`, and the `_meta` key that names the client a message came from is provisional, because the
/// wire a hub carries them on is not specified.
library;

export 'src/session.dart' show ChatSessionHub, chatClientMetaKey;
