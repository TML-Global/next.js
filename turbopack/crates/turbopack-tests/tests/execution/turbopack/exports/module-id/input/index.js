import { __turbopack_module_id__ as id } from "./module.js";

it("should support importing __turbopack_module_id__", () => {
  expect(id).toEndWith("input/module.js [test] (ecmascript)");
})
