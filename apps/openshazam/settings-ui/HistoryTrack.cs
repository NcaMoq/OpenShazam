using System.Globalization;
using System.Text.Json.Serialization;

namespace OpenShazam.Settings;

public sealed class HistoryTrack
{
    public string Title { get; set; } = "";
    public string Artist { get; set; } = "";
    public long RecognizedAtUnixMs { get; set; }
    public string? PageUrl { get; set; }

    [JsonIgnore]
    public string RecognizedAtLabel
    {
        get
        {
            try
            {
                return DateTimeOffset.FromUnixTimeMilliseconds(RecognizedAtUnixMs)
                    .ToLocalTime().ToString("g", CultureInfo.CurrentCulture);
            }
            catch (ArgumentOutOfRangeException)
            {
                return "";
            }
        }
    }
}
