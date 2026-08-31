import { AuthService } from "../lib/auth";

interface UserBadgeProps {
  token: string;
}

export function UserBadge({ token }: UserBadgeProps) {
  const auth = new AuthService();

  function describe(value: string): string {
    const claims = auth.currentUser(value);
    return `user:${claims.userId}`;
  }

  return <span className="badge">{describe(token)}</span>;
}
