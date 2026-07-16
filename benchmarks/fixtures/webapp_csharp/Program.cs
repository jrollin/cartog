// Top-level statements: the implicit entry point (D3 — no explicit Main/class).
using Webapp.Api.V1;
using Webapp.Database;
using Webapp.Services;
using Webapp.Util;

var mainLog = Logger.GetLogger("main");
mainLog.Info("Starting webapp");

var db = new DatabaseConnection("localhost", 5432, "app");
var authService = new AuthenticationService(db);
authService.Initialize();

var controller = new AuthController();
controller.HandleLogin("user@example.com", "secret1");
