//! Vulkan bootstrap: instance, physical/logical device, presentation queue and
//! the allocators shared by every [`crate::gfx::Renderer`].
//!
//! Two entry points exist:
//! * [`GfxContext::for_window`] — enables the platform surface extensions on the
//!   instance and `khr_swapchain` on the device, and requires a queue family
//!   that can both draw and present;
//! * [`GfxContext::headless`] — no surface/swapchain at all, for `--screenshot`.
//!
//! `RIFT_VK_VALIDATION=1` enables the Khronos validation layer (when installed)
//! and routes its messages through `tracing`; without it, rendering must not
//! produce validation errors. The layer is a debug aid, never a hard dependency.

use std::sync::Arc;

use anyhow::{anyhow, Context as _, Result};
use raw_window_handle::HasDisplayHandle;
use vulkano::{
    command_buffer::allocator::StandardCommandBufferAllocator,
    descriptor_set::allocator::StandardDescriptorSetAllocator,
    device::{
        physical::{PhysicalDevice, PhysicalDeviceType},
        Device, DeviceCreateInfo, DeviceExtensions, Queue, QueueCreateInfo, QueueFlags,
    },
    instance::{
        debug::{
            DebugUtilsMessageSeverity, DebugUtilsMessengerCallback, DebugUtilsMessengerCreateInfo,
        },
        Instance, InstanceCreateFlags, InstanceCreateInfo, InstanceExtensions,
    },
    memory::allocator::StandardMemoryAllocator,
    swapchain::Surface,
    VulkanLibrary,
};

/// Everything device-wide, created once per process.
pub(crate) struct GfxContext {
    pub instance: Arc<Instance>,
    pub physical_device: Arc<PhysicalDevice>,
    pub device: Arc<Device>,
    pub queue: Arc<Queue>,
    pub memory_allocator: Arc<StandardMemoryAllocator>,
    pub command_buffer_allocator: Arc<StandardCommandBufferAllocator>,
    pub descriptor_set_allocator: Arc<StandardDescriptorSetAllocator>,
}

impl GfxContext {
    /// Instance + device for presenting to a winit window's surface.
    pub fn for_window(window: &impl HasDisplayHandle) -> Result<Self> {
        let library = VulkanLibrary::new().context("loading the Vulkan library")?;
        let extensions = Surface::required_extensions(window)
            .context("querying the instance extensions required by the window surface")?;
        let instance = create_instance(library, extensions)?;

        let device_extensions = DeviceExtensions {
            khr_swapchain: true,
            ..DeviceExtensions::empty()
        };
        let (physical_device, queue_family_index) = instance
            .enumerate_physical_devices()
            .context("enumerating physical devices")?
            .filter(|p| p.supported_extensions().contains(&device_extensions))
            .filter_map(|p| {
                p.queue_family_properties()
                    .iter()
                    .enumerate()
                    .position(|(index, family)| {
                        family.queue_flags.intersects(QueueFlags::GRAPHICS)
                            && p.presentation_support(index as u32, window)
                                .unwrap_or(false)
                    })
                    .map(|index| (p, index as u32))
            })
            .min_by_key(|(device, _)| device_score(device))
            .ok_or_else(|| {
                anyhow!(
                    "no Vulkan device supports graphics and presentation to this window \
                     (is a Vulkan driver installed?)"
                )
            })?;

        Self::create(
            instance,
            physical_device,
            queue_family_index,
            device_extensions,
        )
    }

    /// Instance + device with no window/surface (offscreen screenshots).
    pub fn headless() -> Result<Self> {
        let library = VulkanLibrary::new().context("loading the Vulkan library")?;
        let instance = create_instance(library, InstanceExtensions::empty())?;

        let (physical_device, queue_family_index) = instance
            .enumerate_physical_devices()
            .context("enumerating physical devices")?
            .filter_map(|p| {
                p.queue_family_properties()
                    .iter()
                    .enumerate()
                    .position(|(_index, family)| {
                        family.queue_flags.intersects(QueueFlags::GRAPHICS)
                    })
                    .map(|index| (p, index as u32))
            })
            .min_by_key(|(device, _)| device_score(device))
            .ok_or_else(|| anyhow!("no Vulkan device exposes a graphics queue"))?;

        Self::create(
            instance,
            physical_device,
            queue_family_index,
            DeviceExtensions::empty(),
        )
    }

    fn create(
        instance: Arc<Instance>,
        physical_device: Arc<PhysicalDevice>,
        queue_family_index: u32,
        enabled_extensions: DeviceExtensions,
    ) -> Result<Self> {
        let (device, mut queues) = Device::new(
            physical_device.clone(),
            DeviceCreateInfo {
                queue_create_infos: vec![QueueCreateInfo {
                    queue_family_index,
                    ..Default::default()
                }],
                enabled_extensions,
                ..Default::default()
            },
        )
        .context("creating the Vulkan logical device")?;
        let queue = queues
            .next()
            .ok_or_else(|| anyhow!("Vulkan device returned no queues"))?;

        tracing::info!(
            device = %physical_device.properties().device_name,
            device_type = ?physical_device.properties().device_type,
            api = ?physical_device.api_version(),
            queue_family = queue_family_index,
            "Vulkan renderer initialised"
        );

        Ok(Self {
            instance,
            physical_device,
            device: device.clone(),
            queue,
            memory_allocator: Arc::new(StandardMemoryAllocator::new_default(device.clone())),
            command_buffer_allocator: Arc::new(StandardCommandBufferAllocator::new(
                device.clone(),
                Default::default(),
            )),
            descriptor_set_allocator: Arc::new(StandardDescriptorSetAllocator::new(
                device,
                Default::default(),
            )),
        })
    }
}

fn create_instance(
    library: Arc<VulkanLibrary>,
    enabled_extensions: InstanceExtensions,
) -> Result<Arc<Instance>> {
    let mut create_info = InstanceCreateInfo {
        flags: InstanceCreateFlags::ENUMERATE_PORTABILITY,
        enabled_extensions,
        ..Default::default()
    };

    if validation_enabled() {
        match install_validation(&library, &mut create_info) {
            Ok(true) => tracing::info!("Vulkan validation layer enabled (RIFT_VK_VALIDATION)"),
            Ok(false) => tracing::warn!(
                "RIFT_VK_VALIDATION is set but VK_LAYER_KHRONOS_validation is not installed"
            ),
            Err(error) => tracing::warn!("could not enable the Vulkan validation layer: {error}"),
        }
    }

    Instance::new(library, create_info).context("creating the Vulkan instance")
}

/// Whether the caller asked for the Khronos validation layer
/// (`RIFT_VK_VALIDATION=1`). Also used to escalate renderer diagnostics (e.g.
/// unknown-texture fallbacks) from debug to warn.
pub(crate) fn validation_enabled() -> bool {
    std::env::var("RIFT_VK_VALIDATION")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Adds the validation layer + a debug-utils messenger to `create_info`.
///
/// Returns `Ok(false)` when the layer is requested but unavailable, so the
/// renderer still starts (a game must not refuse to launch over a debug layer).
fn install_validation(
    library: &Arc<VulkanLibrary>,
    create_info: &mut InstanceCreateInfo,
) -> Result<bool> {
    const LAYER: &str = "VK_LAYER_KHRONOS_validation";
    let available = library
        .layer_properties()
        .context("enumerating Vulkan layers")?
        .any(|layer| layer.name() == LAYER);
    if !available {
        return Ok(false);
    }

    create_info.enabled_layers.push(LAYER.to_owned());
    create_info.enabled_extensions.ext_debug_utils = true;

    // SAFETY: the closure only logs and never calls into Vulkan.
    let callback = unsafe {
        DebugUtilsMessengerCallback::new(|severity, message_type, data| {
            if severity.contains(DebugUtilsMessageSeverity::ERROR) {
                tracing::error!(target: "vulkan", kind = ?message_type, "{}", data.message);
            } else if severity.contains(DebugUtilsMessageSeverity::WARNING) {
                tracing::warn!(target: "vulkan", kind = ?message_type, "{}", data.message);
            } else {
                tracing::debug!(target: "vulkan", kind = ?message_type, "{}", data.message);
            }
        })
    };
    create_info
        .debug_utils_messengers
        .push(DebugUtilsMessengerCreateInfo::user_callback(callback));
    Ok(true)
}

fn device_score(device: &Arc<PhysicalDevice>) -> u32 {
    match device.properties().device_type {
        PhysicalDeviceType::DiscreteGpu => 0,
        PhysicalDeviceType::IntegratedGpu => 1,
        PhysicalDeviceType::VirtualGpu => 2,
        PhysicalDeviceType::Cpu => 3,
        PhysicalDeviceType::Other => 4,
        _ => 9,
    }
}
