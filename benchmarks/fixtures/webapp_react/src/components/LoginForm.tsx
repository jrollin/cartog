import { useState } from "react";
import { AuthService } from "../lib/auth";

interface LoginFormProps {
  onLoggedIn: (token: string) => void;
}

export function LoginForm({ onLoggedIn }: LoginFormProps) {
  const [name, setName] = useState("");
  const auth = new AuthService();

  function onSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const token = auth.login(name);
    onLoggedIn(token);
  }

  return (
    <form onSubmit={onSubmit}>
      <input value={name} onChange={(e) => setName(e.target.value)} />
      <button type="submit">Log in</button>
    </form>
  );
}
