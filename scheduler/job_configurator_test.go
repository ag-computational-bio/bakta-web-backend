package scheduler

import (
	"testing"

	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	db "github.com/ag-computational-bio/bakta-web-backend/database"
)

func Test_species_should_be_quoted(t *testing.T) {

	job := db.Job{}
	job.FastaKey = "irrelevant"

	config := api.JobConfig{}
	config.Species = "test\"; rm -rf /"

	jobstring, _ := createBaktaConf(&job, &config)
	expected := "--tmp-dir /cache --threads 12 --prefix result -o /output --db /db/db --species \"test\\\"; rm -rf /\" --gram ? /data/irrelevant"
	if jobstring != expected {
		t.Errorf("Expected '%v', Got '%v'", expected, jobstring)
	}
}
func Test_genus_should_be_quoted(t *testing.T) {
	job := db.Job{}
	job.FastaKey = "irrelevant"

	config := api.JobConfig{}
	config.Genus = "test\"; rm -rf /"

	jobstring, _ := createBaktaConf(&job, &config)
	expected := "--tmp-dir /cache --threads 12 --prefix result -o /output --db /db/db --genus \"test\\\"; rm -rf /\" --gram ? /data/irrelevant"
	if jobstring != expected {
		t.Errorf("Expected '%v', Got '%v'", expected, jobstring)
	}
}
func Test_strain_should_be_quoted(t *testing.T) {
	job := db.Job{}
	job.FastaKey = "irrelevant"

	config := api.JobConfig{}
	config.Strain = "test\"; rm -rf /"

	jobstring, _ := createBaktaConf(&job, &config)
	expected := "--tmp-dir /cache --threads 12 --prefix result -o /output --db /db/db --strain \"test\\\"; rm -rf /\" --gram ? /data/irrelevant"
	if jobstring != expected {
		t.Errorf("Expected '%v', Got '%v'", expected, jobstring)
	}
}

func Test_locus_should_be_quoted(t *testing.T) {
	job := db.Job{}
	job.FastaKey = "irrelevant"

	config := api.JobConfig{}
	config.Locus = "test\"; rm -rf /"

	jobstring, _ := createBaktaConf(&job, &config)
	expected := "--tmp-dir /cache --threads 12 --prefix result -o /output --db /db/db --locus \"test\\\"; rm -rf /\" --gram ? /data/irrelevant"
	if jobstring != expected {
		t.Errorf("Expected '%v', Got '%v'", expected, jobstring)
	}
}
func Test_locus_tag_should_be_quoted(t *testing.T) {
	job := db.Job{}
	job.FastaKey = "irrelevant"

	config := api.JobConfig{}
	config.LocusTag = "test\"; rm -rf /"

	jobstring, _ := createBaktaConf(&job, &config)
	expected := "--tmp-dir /cache --threads 12 --prefix result -o /output --db /db/db --locus-tag \"test\\\"; rm -rf /\" --gram ? /data/irrelevant"
	if jobstring != expected {
		t.Errorf("Expected '%v', Got '%v'", expected, jobstring)
	}
}

func Test_plasmid_tag_should_be_quoted(t *testing.T) {
	job := db.Job{}
	job.FastaKey = "irrelevant"

	config := api.JobConfig{}
	config.Plasmid = "test\"; rm -rf /"

	jobstring, _ := createBaktaConf(&job, &config)
	expected := "--tmp-dir /cache --threads 12 --prefix result -o /output --db /db/db --plasmid \"test\\\"; rm -rf /\" --gram ? /data/irrelevant"
	if jobstring != expected {
		t.Errorf("Expected '%v', Got '%v'", expected, jobstring)
	}
}

func TestQuote(t *testing.T) {
	testData := []struct {
		input    string
		expected string
	}{
		{"", "\"\""},
		{"test", "\"test\""},
		{"test\" rm -rf /", "\"test\\\" rm -rf /\""},
	}
	for _, data := range testData {
		quoted := quote(data.input)
		if quoted != data.expected {
			t.Errorf("Expected '%v', Got '%v'", data.expected, quoted)
		}
	}
}
