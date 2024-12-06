import { __turbopack_module_id__ as id } from "../components/hello" with {
    "turbopack-transition": "next-dynamic",
    "turbopack-chunking-type": "none"
};
import dynamic from 'next/dynamic';
import somethingElse from 'something-else';
const DynamicComponent = dynamic(()=>import("../components/hello", {
        "turbopack-transition": "next-dynamic"
    }), {
    loadableGenerated: {
        modules: [
            id
        ]
    }
});
somethingElse.dynamic('should not be transformed');
