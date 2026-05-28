import '../auth/service.dart';
import '../auth/tokens.dart';
import '../models/user.dart';
import '../utils/logger.dart';

final _log = getLogger('api');

/// HTTP-ish response envelope.
class Response {
  final int status;
  final String body;
  const Response(this.status, this.body);

  static const Response notFound = Response(404, 'not found');
}

Future<Response> handleLogin(AuthService auth, String userId) async {
  final result = await auth.login(userId);
  return switch (result) {
    AuthSuccess(:final user, :final token) => Response(
        200,
        'welcome ${user.email}; token=${token.masked}',
      ),
    AuthFailure(:final reason) => Response(401, reason),
  };
}

Future<Response> handleSeed(UserRepository users) async {
  await users.save(User.guest());
  _log.info('seeded guest user');
  return const Response(201, 'seeded');
}
