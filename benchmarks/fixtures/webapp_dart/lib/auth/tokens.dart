import 'dart:async';

/// Function type that issues a fresh token for a subject.
typedef TokenIssuer = Future<String> Function(String subject);

/// Decorates a class with a token cache.
mixin TokenCache {
  final Map<String, String> _cache = {};

  String? lookup(String subject) => _cache[subject];

  void remember(String subject, String token) {
    _cache[subject] = token;
  }
}

/// Adds ergonomic helpers to opaque token strings.
extension TokenString on String {
  bool get looksLikeToken => length >= 16 && !contains(' ');
  String get masked => length <= 4 ? '****' : '${substring(0, 4)}…';
}

/// Library-private helper.
String _randomHex(int n) {
  final buf = StringBuffer();
  for (var i = 0; i < n; i++) {
    buf.write((i * 17 % 16).toRadixString(16));
  }
  return buf.toString();
}

/// Default issuer.
Future<String> defaultIssuer(String subject) async {
  return '$subject.${_randomHex(32)}';
}
