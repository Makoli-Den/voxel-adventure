//! Checks that a shader only asks for what its pipeline provides.
//!
//! naga's validator proves a module is well formed, but not that it fits our pipeline.
//! A shader that declares a larger push-constant block, an unknown vertex input or a
//! descriptor resource the layout lacks would make pipeline creation invalid, which Vulkan
//! does not have to report and a driver may crash on. These checks turn that into an error.
//!
//! Every check compares against the pipeline's own [`PipelineInterface`] from
//! `crate::interface`. Checked: descriptor resources (every buffer, image or sampler the
//! module declares, used or not, must be one the interface lists, at its set and binding,
//! of its kind, in its stage), push constants (only
//! in the interface's stage, at most its size in bytes), vertex inputs (locations and
//! exact types of the interface's attributes, built-ins limited to vertex and instance
//! index), and every fragment input location against a vertex output of the same scalar
//! or vector type. Not checked: interpolation qualifiers, and fragment outputs (writes
//! to a location without a color attachment are discarded, which is valid). Matrices,
//! arrays and structs are rejected as fragment inputs rather than compared.

use naga::proc::Layouter;
use naga::{
    AddressSpace, Binding, BuiltIn, EntryPoint, GlobalVariable, Handle, ImageClass, ImageDimension,
    Module, Scalar, ScalarKind, ShaderStage, Type, TypeInner,
};

use crate::interface::{PipelineInterface, ResourceKind, Stage};

/// A scalar or vector type: the only types the checks compare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shape {
    components: u32,
    scalar: Scalar,
}

/// A value crossing a stage boundary: its binding and type.
type Io = (Binding, Handle<Type>);

/// Checks one stage's module against what `interface` provides for that stage.
pub(super) fn check_stage(
    module: &Module,
    stage: ShaderStage,
    interface: &PipelineInterface,
) -> Result<(), String> {
    check_resources(module, stage, interface)?;
    check_push_constants(module, stage, interface)?;
    if stage == ShaderStage::Vertex {
        check_vertex_inputs(module, interface)?;
    }
    Ok(())
}

/// Checks that every fragment input is written by the vertex stage with the same type.
pub(super) fn check_link(vertex: &Module, fragment: &Module) -> Result<(), String> {
    let outputs = outputs(vertex, entry_point(vertex, ShaderStage::Vertex)?);
    for (binding, ty) in inputs(fragment, entry_point(fragment, ShaderStage::Fragment)?) {
        let Binding::Location { location, .. } = binding else {
            continue; // built-ins come from the rasterizer, not the vertex shader
        };
        let Some(input) = shape(fragment, ty) else {
            return Err(format!(
                "fragment input at location {location} must be a scalar or a vector"
            ));
        };
        let written = outputs.iter().find_map(|(binding, ty)| match binding {
            Binding::Location { location: l, .. } if *l == location => Some(shape(vertex, *ty)),
            _ => None,
        });
        match written {
            None => {
                return Err(format!(
                    "fragment input at location {location} is not written by the vertex shader"
                ));
            }
            Some(output) if output != Some(input) => {
                return Err(format!(
                    "fragment input at location {location} has a different type than the vertex output"
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Every descriptor resource the module declares must be one the interface provides to
/// this stage, declared with its exact kind. Unused declarations count too: rejecting them
/// is simpler than proving they are unused, and costs a pack author nothing.
fn check_resources(
    module: &Module,
    stage: ShaderStage,
    interface: &PipelineInterface,
) -> Result<(), String> {
    for (_, global) in module.global_variables.iter() {
        if is_resource(global.space) {
            check_resource(module, global, stage, interface)?;
        }
    }
    Ok(())
}

/// Whether a global in `space` is a descriptor resource: a buffer, an image or a sampler.
fn is_resource(space: AddressSpace) -> bool {
    matches!(
        space,
        AddressSpace::Uniform | AddressSpace::Storage { .. } | AddressSpace::Handle
    )
}

/// Checks one descriptor resource against the interface.
fn check_resource(
    module: &Module,
    global: &GlobalVariable,
    stage: ShaderStage,
    interface: &PipelineInterface,
) -> Result<(), String> {
    let name = global.name.as_deref().unwrap_or("(unnamed)");
    let Some(binding) = &global.binding else {
        return Err(format!(
            "descriptor resource `{name}` has no set and binding"
        ));
    };
    let (set, index) = (binding.group, binding.binding);
    let Some(provided) = interface.resource(set, index) else {
        return Err(format!(
            "descriptor resource `{name}` at set {set}, binding {index} does not exist; {}",
            provided_resources(interface)
        ));
    };
    if pipeline_stage(stage) != Some(provided.stage) {
        return Err(format!(
            "descriptor resource `{name}` at set {set}, binding {index} is only available in the {} stage",
            provided.stage.name()
        ));
    }
    if !is_kind(provided.kind, global.space, &module.types[global.ty].inner) {
        return Err(format!(
            "descriptor resource `{name}` at set {set}, binding {index} must be declared as `{}`",
            provided.kind.glsl_type()
        ));
    }
    Ok(())
}

/// Lists the interface's resources for an error message.
fn provided_resources(interface: &PipelineInterface) -> String {
    if interface.resources.is_empty() {
        return String::from("the pipeline has no descriptor resources");
    }
    let list: Vec<String> = interface
        .resources
        .iter()
        .map(|r| {
            format!(
                "set {}, binding {} (`{}`, {} stage)",
                r.set,
                r.binding,
                r.kind.glsl_type(),
                r.stage.name()
            )
        })
        .collect();
    format!("the pipeline provides {}", list.join("; "))
}

/// Whether a global in `space` with type `inner` is exactly a resource of `kind`.
fn is_kind(kind: ResourceKind, space: AddressSpace, inner: &TypeInner) -> bool {
    if space != AddressSpace::Handle {
        return false; // buffers: no pipeline provides one
    }
    match kind {
        ResourceKind::SampledImage2DArray => matches!(
            inner,
            TypeInner::Image {
                dim: ImageDimension::D2,
                arrayed: true,
                class: ImageClass::Sampled {
                    kind: ScalarKind::Float,
                    multi: false,
                },
            }
        ),
        ResourceKind::Sampler => matches!(inner, TypeInner::Sampler { comparison: false }),
    }
}

/// Push constants exist only in the interface's stage, and only as many bytes as it has.
fn check_push_constants(
    module: &Module,
    stage: ShaderStage,
    interface: &PipelineInterface,
) -> Result<(), String> {
    let provided = interface.push_constants;
    match push_constant_size(module)? {
        None => Ok(()),
        Some(_) if pipeline_stage(stage) != Some(provided.stage) => Err(format!(
            "push constants are only available in the {} stage",
            provided.stage.name()
        )),
        Some(size) if size > provided.size => Err(format!(
            "push constants use {size} bytes, the pipeline provides {} bytes",
            provided.size
        )),
        Some(_) => Ok(()),
    }
}

/// The pipeline stage a naga stage stands for, or `None` for the stages no pipeline of
/// the renderer has (compute, task and mesh shaders), which then match no interface.
fn pipeline_stage(stage: ShaderStage) -> Option<Stage> {
    match stage {
        ShaderStage::Vertex => Some(Stage::Vertex),
        ShaderStage::Fragment => Some(Stage::Fragment),
        ShaderStage::Compute | ShaderStage::Task | ShaderStage::Mesh => None,
    }
}

/// Total size of all push-constant blocks, or `None` if the module declares none.
fn push_constant_size(module: &Module) -> Result<Option<u32>, String> {
    let mut layouter = Layouter::default();
    layouter
        .update(module.to_ctx())
        .map_err(|error| format!("cannot compute type sizes: {error}"))?;
    let sizes = module
        .global_variables
        .iter()
        .filter(|(_, global)| global.space == AddressSpace::Immediate)
        .map(|(_, global)| layouter[global.ty].size);
    Ok(sizes.reduce(|total, size| total.saturating_add(size)))
}

/// Vertex inputs must be attributes the pipeline feeds, declared with their exact type.
fn check_vertex_inputs(module: &Module, interface: &PipelineInterface) -> Result<(), String> {
    let entry = entry_point(module, ShaderStage::Vertex)?;
    for (binding, ty) in inputs(module, entry) {
        match binding {
            Binding::BuiltIn(BuiltIn::VertexIndex | BuiltIn::InstanceIndex) => {}
            Binding::BuiltIn(other) => {
                return Err(format!("vertex input built-in {other:?} is not available"));
            }
            Binding::Location { location, .. } => {
                check_attribute(interface, location, shape(module, ty))?;
            }
        }
    }
    Ok(())
}

/// Checks one vertex input against the attribute the pipeline feeds at `location`.
fn check_attribute(
    interface: &PipelineInterface,
    location: u32,
    declared: Option<Shape>,
) -> Result<(), String> {
    let Some(attribute) = interface.attribute(location) else {
        let provided: Vec<String> = interface
            .attributes
            .iter()
            .map(|a| a.location.to_string())
            .collect();
        return Err(format!(
            "vertex input at location {location} does not exist; the pipeline provides locations {}",
            provided.join(", ")
        ));
    };
    let expected = Shape {
        components: attribute.format.components(),
        scalar: attribute.format.shader_scalar(),
    };
    if declared != Some(expected) {
        return Err(format!(
            "vertex input at location {location} must be declared as `{}`",
            attribute.format.glsl_type()
        ));
    }
    Ok(())
}

/// The entry point of `stage`; a GLSL module has exactly one.
fn entry_point(module: &Module, stage: ShaderStage) -> Result<&EntryPoint, String> {
    module
        .entry_points
        .iter()
        .find(|entry| entry.stage == stage)
        .ok_or_else(|| format!("no {stage:?} entry point"))
}

/// All inputs of an entry point, with struct arguments flattened to their members.
fn inputs(module: &Module, entry: &EntryPoint) -> Vec<Io> {
    let mut io = Vec::new();
    for argument in &entry.function.arguments {
        flatten(module, argument.ty, argument.binding.as_ref(), &mut io);
    }
    io
}

/// All outputs of an entry point, with a struct result flattened to its members.
fn outputs(module: &Module, entry: &EntryPoint) -> Vec<Io> {
    let mut io = Vec::new();
    if let Some(result) = &entry.function.result {
        flatten(module, result.ty, result.binding.as_ref(), &mut io);
    }
    io
}

/// Collects bound values: a value with a binding as is, an unbound struct by its members.
fn flatten(module: &Module, ty: Handle<Type>, binding: Option<&Binding>, io: &mut Vec<Io>) {
    match binding {
        Some(binding) => io.push((binding.clone(), ty)),
        None => {
            if let TypeInner::Struct { members, .. } = &module.types[ty].inner {
                for member in members {
                    flatten(module, member.ty, member.binding.as_ref(), io);
                }
            }
        }
    }
}

/// The shape of a scalar or vector type, `None` for anything else.
fn shape(module: &Module, ty: Handle<Type>) -> Option<Shape> {
    match module.types[ty].inner {
        TypeInner::Scalar(scalar) => Some(Shape {
            components: 1,
            scalar,
        }),
        TypeInner::Vector { size, scalar } => Some(Shape {
            components: size as u32,
            scalar,
        }),
        _ => None,
    }
}
