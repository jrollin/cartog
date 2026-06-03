import Foundation

let db = DatabaseConnection(dsn: "postgres://localhost/webapp")
db.connect()

let service = AuthenticationService(db: db)
let handlers = AuthHandlers(service: service)
let middleware = AuthMiddleware()
let routes = AuthRoutes(handlers: handlers, middleware: middleware)
routes.register()

let token = routes.dispatchLogin(email: "user@example.com", password: "secret")
getLogger("main").info("issued token \(token)")

db.close()
