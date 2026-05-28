/// User role with associated permission level.
enum Role {
  guest(0),
  member(1),
  admin(10);

  final int level;
  const Role(this.level);

  bool canAdmin() => level >= Role.admin.level;
}
