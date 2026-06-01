//! Curated provider-type → support-service hints.
//!
//! These hints supplement the seed catalog. They are tiny and stable.
//! Used as tiebreakers / explanation reasons in ranking; the resolver
//! ultimately defers to the seed's `resource_types`.

pub fn hint_display_name(resource_type: &str) -> Option<&'static str> {
    match resource_type {
        "Microsoft.Compute/virtualMachines" => Some("Virtual Machine running Linux"),
        "Microsoft.Compute/virtualMachineScaleSets" => Some("Virtual Machine Scale Sets"),
        "Microsoft.ContainerService/managedClusters" => Some("Azure Kubernetes Service"),
        "Microsoft.Web/sites" => Some("App Service"),
        "Microsoft.Sql/servers/databases" => Some("Azure SQL Database"),
        "Microsoft.DBforPostgreSQL/flexibleServers" => Some("Azure Database for PostgreSQL"),
        "Microsoft.DBforMySQL/flexibleServers" => Some("Azure Database for MySQL"),
        "Microsoft.Network/applicationGateways" => Some("Application Gateway"),
        "Microsoft.Network/loadBalancers" => Some("Load Balancer"),
        "Microsoft.Network/virtualNetworks" => Some("Virtual Network"),
        "Microsoft.Network/privateEndpoints" => Some("Private Link"),
        "Microsoft.KeyVault/vaults" => Some("Key Vault"),
        "Microsoft.Storage/storageAccounts" => Some("Storage Account"),
        "Microsoft.ServiceBus/namespaces" => Some("Service Bus"),
        "Microsoft.EventHub/namespaces" => Some("Event Hubs"),
        "Microsoft.DocumentDB/databaseAccounts" => Some("Azure Cosmos DB"),
        "Microsoft.OperationalInsights/workspaces" => Some("Log Analytics"),
        "Microsoft.Insights/components" => Some("Application Insights"),
        "Microsoft.ContainerRegistry/registries" => Some("Azure Container Registry"),
        _ => None,
    }
}
