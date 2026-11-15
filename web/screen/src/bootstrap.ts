import { startPage } from "./page";

const root = document.querySelector<HTMLElement>("main");
if (root) startPage(root, document.baseURI);
