import '../models/user.dart';
import 'tokens.dart';

/// Outcome of an authentication attempt.
sealed class AuthResult {
  const AuthResult();
}

class AuthSuccess extends AuthResult {
  final User user;
  final String token;
  const AuthSuccess(this.user, this.token);
}

class AuthFailure extends AuthResult {
  final String reason;
  const AuthFailure(this.reason);
}

/// Authenticates users and issues tokens.
class AuthService with TokenCache {
  final UserRepository _users;
  final TokenIssuer _issue;

  AuthService(this._users, {TokenIssuer? issuer})
      : _issue = issuer ?? defaultIssuer;

  /// Named constructor wiring the default in-memory repository.
  AuthService.inMemory() : this(UserRepository());

  Future<AuthResult> login(String userId) async {
    final user = await _users.findById(userId);
    if (user == null) {
      return const AuthFailure('unknown user');
    }
    final cached = lookup(userId);
    if (cached != null) {
      return AuthSuccess(user, cached);
    }
    final token = await _issue(userId);
    remember(userId, token);
    return AuthSuccess(user, token);
  }
}
