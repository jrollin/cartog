import { generateToken, validateToken, TokenClaims } from "./tokens";

export interface User {
  id: string;
  name: string;
}

export class AuthService {
  login(name: string): string {
    const user = this.findUser(name);
    return generateToken(user.id);
  }

  currentUser(token: string): TokenClaims {
    return validateToken(token);
  }

  private findUser(name: string): User {
    return { id: name.toLowerCase(), name };
  }
}
