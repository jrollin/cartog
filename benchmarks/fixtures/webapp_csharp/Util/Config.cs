// Block-namespace form (D3) plus a nested type.
namespace Webapp.Util
{
    /// <summary>
    /// Application configuration with a nested Builder type.
    /// </summary>
    public class Config
    {
        public string DatabaseUrl { get; set; }

        /// <summary>
        /// Fluent builder for <see cref="Config"/> (nested type → dotted qname).
        /// </summary>
        public class Builder
        {
            private readonly Config _config = new Config();

            public Builder WithDatabaseUrl(string url)
            {
                _config.DatabaseUrl = url;
                return this;
            }

            public Config Build()
            {
                return _config;
            }
        }
    }
}
