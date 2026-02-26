# User model.

class User
  attr_reader :id, :email, :is_admin

  def initialize(id:, email:, password_hash:, is_admin: false)
    @id = id
    @email = email
    @password_hash = password_hash
    @is_admin = is_admin
  end

  def check_password(password)
    @password_hash == Digest::SHA256.hexdigest(password)
  end

  def set_password(new_password)
    @password_hash = Digest::SHA256.hexdigest(new_password)
  end

  def self.find_by_email(db, email)
    db.query('SELECT * FROM users WHERE email = ?', email)
  end

  def self.find_by_id(db, id)
    db.query('SELECT * FROM users WHERE id = ?', id)
  end

  def self.find_all(db)
    db.query('SELECT * FROM users')
  end
end
