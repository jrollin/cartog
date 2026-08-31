import { useState } from "react";
import { LoginForm } from "./components/LoginForm";
import { UserBadge } from "./components/UserBadge";

export function App() {
  const [token, setToken] = useState("");

  function onLoggedIn(value: string) {
    setToken(value);
  }

  return (
    <main>
      <LoginForm onLoggedIn={onLoggedIn} />
      {token ? <UserBadge token={token} /> : null}
    </main>
  );
}
