import 'role.dart';

/// Domain user.
class User {
  final String id;
  final String email;
  final Role role;

  const User({required this.id, required this.email, required this.role});

  /// Convenience factory for an anonymous guest.
  factory User.guest() =>
      const User(id: '0', email: 'guest@example.com', role: Role.guest);

  bool get isAdmin => role.canAdmin();

  @override
  String toString() => 'User($id, $email, ${role.name})';
}

/// Generic repository contract.
abstract class Repository<T> {
  Future<T?> findById(String id);
  Future<List<T>> findAll();
}

/// In-memory user repository.
class UserRepository implements Repository<User> {
  final Map<String, User> _store = {};

  @override
  Future<User?> findById(String id) async => _store[id];

  @override
  Future<List<User>> findAll() async => _store.values.toList();

  Future<void> save(User user) async {
    _store[user.id] = user;
  }
}
