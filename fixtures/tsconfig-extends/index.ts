// This should resolve to ./lib/app.ts because child tsconfig
// overrides the @/* path mapping
import { app } from "@/app";

export { app };
