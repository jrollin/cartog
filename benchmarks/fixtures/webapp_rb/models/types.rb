# Shared type definitions as modules of constants.

module UserRole
  USER = 'user'
  ADMIN = 'admin'
  MODERATOR = 'moderator'
end

module EventType
  USER_REGISTERED = 'user.registered'
  LOGIN_SUCCESS = 'auth.login_success'
  LOGIN_FAILED = 'auth.login_failed'
  PAYMENT_COMPLETED = 'payment.completed'
  PAYMENT_REFUNDED = 'payment.refunded'
  PASSWORD_CHANGED = 'auth.password_changed'
end

module NotificationChannel
  EMAIL = 'email'
  SMS = 'sms'
  PUSH = 'push'
  IN_APP = 'in_app'
end
