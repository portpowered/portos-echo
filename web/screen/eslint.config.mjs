import tseslint from "typescript-eslint";

export default tseslint.config(
    { ignores: ["node_modules/**", "dist/**", "test-results/**"] },
    ...tseslint.configs.recommended,
    {
        files: ["**/*.mjs"],
        languageOptions: { ecmaVersion: 2022, sourceType: "module" },
    },
);
