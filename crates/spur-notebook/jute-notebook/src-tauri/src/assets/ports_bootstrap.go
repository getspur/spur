
!*go get github.com/apache/arrow-go/v18
!*go get github.com/janpfeifer/gonb/gonbui

// --- SPUR port helper bootstrap ---
import "encoding/json"
import "errors"
import "fmt"
import "html"
import "os"
import "path/filepath"
import "strings"

import "github.com/apache/arrow-go/v18/arrow"
import "github.com/apache/arrow-go/v18/arrow/ipc"
import "github.com/janpfeifer/gonb/gonbui"
import "github.com/janpfeifer/gonb/gonbui/protocol"

const portMime = "application/vnd.spur.port+json"
const portsDirName = "ports"
const portFileVersionSeparator = "@v"
const portArrowFileExtension = "arrow"
const portManifestFileName = "manifest.json"
const portManifestTempPrefix = "manifest.json."
const portTempFileSuffix = ".tmp"
const portManifestPortsKey = "ports"
const portManifestPathKey = "path"
const portManifestVersionKey = "version"
const portManifestSchemaKey = "schema"
const portInitialVersion uint64 = 0
const portVersionIncrement uint64 = 1
const portReservedCurrentDir = "."
const portReservedParentDir = ".."
const portForbiddenSlash = "/"
const portForbiddenBackslash = "\\"
const portForbiddenNul = "\u0000"

type spurPorts struct {
    root string
    portsDir string
    manifestPath string
    mime string
}

type spurManifest struct {
    Ports map[string]spurManifestEntry `json:"ports"`
}

type spurManifestEntry struct {
    Path string `json:"path"`
    Version uint64 `json:"version"`
    Schema any `json:"schema"`
}

func newSpurPorts(root, portsDir, manifestPath, mime string) *spurPorts {
    _ = os.MkdirAll(portsDir, 0o755)
    return &spurPorts{
        root: root,
        portsDir: portsDir,
        manifestPath: manifestPath,
        mime: mime,
    }
}

func (s *spurPorts) Put(port string, batch arrow.Record) (map[string]any, error) {
    if batch == nil {
        return nil, errors.New("SPUR port batch cannot be nil")
    }
    if err := s.validatePort(port); err != nil {
        return nil, err
    }
    if err := os.MkdirAll(s.portsDir, 0o755); err != nil {
        return nil, err
    }

    manifest, err := s.loadManifest()
    if err != nil {
        return nil, err
    }
    schema, err := spurSchemaJSON(batch.Schema(), port)
    if err != nil {
        return nil, err
    }
    previousVersion := portInitialVersion
    if entry, ok := manifest.Ports[port]; ok {
        previousVersion = entry.Version
    }
    version := previousVersion + portVersionIncrement
    arrowPath := filepath.Join(
        s.portsDir,
        fmt.Sprintf("%s%s%d.%s", port, portFileVersionSeparator, version, portArrowFileExtension),
    )

    if err := s.writeArrowFile(arrowPath, batch); err != nil {
        return nil, err
    }

    manifest.Ports[port] = spurManifestEntry{
        Path: arrowPath,
        Version: version,
        Schema: schema,
    }
    if err := s.storeManifest(manifest); err != nil {
        return nil, err
    }

    payload := map[string]any{
        "port": port,
        portManifestVersionKey: version,
        portManifestSchemaKey: schema,
    }
    s.displayPort(port, version, schema, batch)
    return payload, nil
}

func (s *spurPorts) Get(port string) ([]arrow.Record, error) {
    if err := s.validatePort(port); err != nil {
        return nil, err
    }
    manifest, err := s.loadManifest()
    if err != nil {
        return nil, err
    }
    entry, ok := manifest.Ports[port]
    if !ok {
        return nil, fmt.Errorf("SPUR port has not been written: %s", port)
    }

    file, err := os.Open(entry.Path)
    if err != nil {
        return nil, err
    }
    defer file.Close()
    reader, err := ipc.NewFileReader(file)
    if err != nil {
        return nil, err
    }
    defer reader.Close()

    records := make([]arrow.Record, 0, reader.NumRecords())
    for i := 0; i < reader.NumRecords(); i++ {
        record, err := reader.RecordAt(i)
        if err != nil {
            return nil, err
        }
        records = append(records, record)
    }
    return records, nil
}

func (s *spurPorts) writeArrowFile(path string, batch arrow.Record) error {
    file, err := os.Create(path)
    if err != nil {
        return err
    }
    writer, err := ipc.NewFileWriter(file, ipc.WithSchema(batch.Schema()))
    if err != nil {
        _ = file.Close()
        return err
    }
    if err := writer.Write(batch); err != nil {
        _ = writer.Close()
        _ = file.Close()
        return err
    }
    if err := writer.Close(); err != nil {
        _ = file.Close()
        return err
    }
    return file.Close()
}

func (s *spurPorts) loadManifest() (spurManifest, error) {
    manifest := spurManifest{Ports: map[string]spurManifestEntry{}}
    data, err := os.ReadFile(s.manifestPath)
    if errors.Is(err, os.ErrNotExist) {
        return manifest, nil
    }
    if err != nil {
        return manifest, err
    }
    if err := json.Unmarshal(data, &manifest); err != nil {
        return manifest, err
    }
    if manifest.Ports == nil {
        manifest.Ports = map[string]spurManifestEntry{}
    }
    return manifest, nil
}

func (s *spurPorts) storeManifest(manifest spurManifest) error {
    tmp, err := os.CreateTemp(
        s.portsDir,
        fmt.Sprintf("%s*%s", portManifestTempPrefix, portTempFileSuffix),
    )
    if err != nil {
        return err
    }
    tmpPath := tmp.Name()
    cleanup := true
    defer func() {
        if cleanup {
            _ = os.Remove(tmpPath)
        }
    }()

    encoder := json.NewEncoder(tmp)
    encoder.SetIndent("", "  ")
    if err := encoder.Encode(manifest); err != nil {
        _ = tmp.Close()
        return err
    }
    if err := tmp.Close(); err != nil {
        return err
    }
    if err := os.Rename(tmpPath, s.manifestPath); err != nil {
        return err
    }
    cleanup = false
    return nil
}

func (s *spurPorts) validatePort(port string) error {
    if port == "" {
        return errors.New("SPUR port name cannot be empty")
    }
    if port == portReservedCurrentDir ||
        port == portReservedParentDir ||
        strings.Contains(port, portForbiddenSlash) ||
        strings.Contains(port, portForbiddenBackslash) ||
        strings.Contains(port, portForbiddenNul) {
        return fmt.Errorf("SPUR port name is not valid for an on-disk port file: %s", port)
    }
    return nil
}

func (s *spurPorts) displayPort(port string, version uint64, schema map[string]any, batch arrow.Record) {
    gonbui.SendData(&protocol.DisplayData{
        Data: map[protocol.MIMEType]any{
            protocol.MIMEType(s.mime): map[string]any{
                "port": port,
                portManifestVersionKey: version,
                portManifestSchemaKey: schema,
            },
            protocol.MIMETextHTML: s.previewHTML(port, version, batch),
        },
    })
    gonbui.Sync()
}

func (s *spurPorts) previewHTML(port string, version uint64, batch arrow.Record) string {
    return fmt.Sprintf(
        "<div><strong>SPUR port</strong> <code>%s</code> <span>v%d</span><p>%d rows x %d columns</p></div>",
        html.EscapeString(port),
        version,
        batch.NumRows(),
        batch.NumCols(),
    )
}

func spurSchemaJSON(schema *arrow.Schema, port string) (map[string]any, error) {
    if schema == nil {
        return nil, fmt.Errorf("SPUR port %q: Arrow schema cannot be nil", port)
    }
    fields := make([]map[string]any, 0, schema.NumFields())
    for _, field := range schema.Fields() {
        dataType, err := spurDataTypeJSON(field.Type, port)
        if err != nil {
            return nil, err
        }
        fields = append(fields, map[string]any{
            "name": field.Name,
            "data_type": dataType,
            "nullable": field.Nullable,
            "dict_id": 0,
            "dict_is_ordered": false,
            "metadata": spurMetadataJSON(field.Metadata),
        })
    }
    return map[string]any{
        "fields": fields,
        "metadata": spurMetadataJSON(schema.Metadata()),
    }, nil
}

func spurMetadataJSON(metadata arrow.Metadata) map[string]string {
    output := map[string]string{}
    for key, value := range metadata.ToMap() {
        output[key] = value
    }
    return output
}

func spurDataTypeJSON(dataType arrow.DataType, port string) (any, error) {
    switch t := dataType.(type) {
    case *arrow.BooleanType:
        return "Boolean", nil
    case *arrow.Int8Type:
        return "Int8", nil
    case *arrow.Int16Type:
        return "Int16", nil
    case *arrow.Int32Type:
        return "Int32", nil
    case *arrow.Int64Type:
        return "Int64", nil
    case *arrow.Uint8Type:
        return "UInt8", nil
    case *arrow.Uint16Type:
        return "UInt16", nil
    case *arrow.Uint32Type:
        return "UInt32", nil
    case *arrow.Uint64Type:
        return "UInt64", nil
    case *arrow.Float16Type:
        return "Float16", nil
    case *arrow.Float32Type:
        return "Float32", nil
    case *arrow.Float64Type:
        return "Float64", nil
    case *arrow.StringType:
        return "Utf8", nil
    case *arrow.LargeStringType:
        return "LargeUtf8", nil
    case *arrow.BinaryType:
        return "Binary", nil
    case *arrow.LargeBinaryType:
        return "LargeBinary", nil
    case *arrow.NullType:
        return "Null", nil
    case *arrow.Date32Type:
        return "Date32", nil
    case *arrow.Date64Type:
        return "Date64", nil
    case *arrow.TimestampType:
        unit, err := spurTimeUnitJSON(t.Unit, port, dataType)
        if err != nil {
            return nil, err
        }
        var timezone any
        if t.TimeZone != "" {
            timezone = t.TimeZone
        }
        return map[string]any{"Timestamp": []any{unit, timezone}}, nil
    case *arrow.Time32Type:
        unit, err := spurTimeUnitJSON(t.Unit, port, dataType)
        if err != nil {
            return nil, err
        }
        return map[string]any{"Time32": unit}, nil
    case *arrow.Time64Type:
        unit, err := spurTimeUnitJSON(t.Unit, port, dataType)
        if err != nil {
            return nil, err
        }
        return map[string]any{"Time64": unit}, nil
    case *arrow.Decimal128Type:
        return map[string]any{"Decimal128": []any{t.GetPrecision(), t.GetScale()}}, nil
    case *arrow.Decimal256Type:
        return map[string]any{"Decimal256": []any{t.GetPrecision(), t.GetScale()}}, nil
    case *arrow.DictionaryType:
        index, err := spurDataTypeJSON(t.IndexType, port)
        if err != nil {
            return nil, err
        }
        value, err := spurDataTypeJSON(t.ValueType, port)
        if err != nil {
            return nil, err
        }
        return map[string]any{"Dictionary": []any{index, value}}, nil
    default:
        return nil, fmt.Errorf("SPUR port %q: unsupported Arrow type for manifest schema: %s", port, dataType)
    }
}

func spurTimeUnitJSON(unit arrow.TimeUnit, port string, dataType arrow.DataType) (string, error) {
    switch unit {
    case arrow.Second:
        return "Second", nil
    case arrow.Millisecond:
        return "Millisecond", nil
    case arrow.Microsecond:
        return "Microsecond", nil
    case arrow.Nanosecond:
        return "Nanosecond", nil
    default:
        return "", fmt.Errorf("SPUR port %q: unsupported Arrow time unit for manifest schema: %s", port, dataType)
    }
}

var spurPortRoot = func() string {
    root := os.Getenv("SPUR_NOTEBOOK_PORT_ROOT")
    if root == "" {
        panic("SPUR_NOTEBOOK_PORT_ROOT is not set")
    }
    return root
}()
var spur = newSpurPorts(
    spurPortRoot,
    filepath.Join(spurPortRoot, portsDirName),
    filepath.Join(spurPortRoot, portsDirName, portManifestFileName),
    portMime,
)
// --- end SPUR port helper bootstrap ---
