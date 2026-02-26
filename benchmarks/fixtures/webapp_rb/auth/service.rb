# Authentication service.

require_relative 'tokens'
require_relative '../models/user'
require_relative '../utils/logging'

class BaseService
  # Base service with common utilities.

  def initialize
    @initialized = true
    @logger = Logging.get_logger(self.class.name)
  end

  def _log(message)
    @logger.info("[#{self.class.name}] #{message}")
  end
end

class AuthService < BaseService
  # Handles user authentication flows.

  def initialize(db)
    super()
    @db = db
  end

  def login(email, password)
    user = _find_user(email)
    if user && user.check_password(password)
      _log("Login successful for #{email}")
      return generate_token(user)
    end
    _log("Login failed for #{email}")
    nil
  end

  def logout(token)
    revoke_token(token)
  end

  def get_current_user(token)
    validate_token(token)
  end

  def _find_user(email)
    User.find_by_email(@db, email)
  end

  def change_password(token, old_pw, new_pw)
    user = validate_token(token)
    if user && user.check_password(old_pw)
      user.set_password(new_pw)
      return true
    end
    false
  end
end

class AdminService < AuthService
  # Extended auth service for admin operations.

  def impersonate(admin_token, user_id)
    admin = get_current_user(admin_token)
    if admin && admin.is_admin
      target = User.find_by_id(@db, user_id)
      if target
        _log("Admin #{admin.email} impersonating #{target.email}")
        return generate_token(target)
      end
    end
    raise PermissionError, 'Not authorized'
  end

  def list_all_users(admin_token)
    admin = get_current_user(admin_token)
    return User.find_all(@db) if admin && admin.is_admin

    raise PermissionError, 'Not authorized'
  end
end
