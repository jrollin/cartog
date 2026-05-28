import '../lib/webapp.dart';

Future<void> main(List<String> args) async {
  final users = UserRepository();
  final auth = AuthService(users);
  await handleSeed(users);
  final response = await handleLogin(auth, '0');
  // ignore: avoid_print
  print('${response.status} ${response.body}');
}
